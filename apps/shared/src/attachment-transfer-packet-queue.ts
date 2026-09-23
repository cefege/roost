// FIFO packet ownership for one ordered attachment data-channel direction.
// It retains source buffers until transport acceptance and releases each quota
// reservation once, so buffered native sends cannot duplicate a fragment.

import {
  ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES,
  ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES,
} from "./attachment-transfer.ts";
import {
  encodeAttachmentTransferPacket,
  AttachmentTransferPacketError,
  type AttachmentTransferPacketDirection,
  type AttachmentTransferPacketQuota,
} from "./attachment-transfer-packets.ts";

interface QueuedMessage {
  readonly messageId: number;
  readonly bytes: Uint8Array;
  offsetBytes: number;
  released: boolean;
}

interface PendingFragment {
  readonly message: QueuedMessage;
  readonly payloadBytes: number;
  readonly fragment: AttachmentTransferPacketQueueFragment;
  committed: boolean;
}

/** A framed packet remains owned by its queue until the transport accepts it. */
export interface AttachmentTransferPacketQueueFragment {
  readonly bytes: Uint8Array;
  readonly direction: AttachmentTransferPacketDirection;
  readonly messageId: number;
  readonly final: boolean;
  /** Advance exactly once after browser or native send accepts this packet. */
  commit(): void;
}

/** FIFO source-buffer ownership for one ordered attachment channel direction. */
export class AttachmentTransferPacketQueue {
  private readonly messages: QueuedMessage[] = [];
  private pending: PendingFragment | null = null;
  private nextMessageId = 1;
  private retainedBytes = 0;
  private closed = false;

  constructor(
    readonly direction: AttachmentTransferPacketDirection,
    private readonly quota: AttachmentTransferPacketQuota,
  ) {}

  get queuedBytes(): number {
    return this.retainedBytes;
  }

  get messageCount(): number {
    return this.messages.length;
  }

  get isClosed(): boolean {
    return this.closed;
  }

  /** Transfers source-buffer ownership without copying or pre-fragmenting. */
  enqueue(bytes: Uint8Array): boolean {
    if (this.closed) throw new AttachmentTransferPacketError("closed");
    if (
      bytes.byteLength < 1
      || bytes.byteLength > ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES
    ) {
      throw new AttachmentTransferPacketError("message-size");
    }
    if (this.nextMessageId === 0) throw new AttachmentTransferPacketError("message-id-wrap");
    if (!this.quota.reserve(this.direction, bytes.byteLength)) return false;
    try {
      const message: QueuedMessage = {
        messageId: this.nextMessageId,
        bytes,
        offsetBytes: 0,
        released: false,
      };
      this.messages.push(message);
      this.retainedBytes += bytes.byteLength;
      this.nextMessageId = message.messageId === 0xffff_ffff ? 0 : message.messageId + 1;
      return true;
    } catch {
      this.quota.release(this.direction, bytes.byteLength);
      throw new AttachmentTransferPacketError("allocation");
    }
  }

  /** Materializes one outer packet and caches it until accepted-once commit. */
  nextFragment(): AttachmentTransferPacketQueueFragment | null {
    if (this.pending) return this.pending.fragment;
    const message = this.messages[0];
    if (!message) return null;
    const payloadBytes = Math.min(
      ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES,
      message.bytes.byteLength - message.offsetBytes,
    );
    let bytes: Uint8Array;
    try {
      bytes = encodeAttachmentTransferPacket({
        messageId: message.messageId,
        totalBytes: message.bytes.byteLength,
        offsetBytes: message.offsetBytes,
      }, message.bytes.subarray(message.offsetBytes, message.offsetBytes + payloadBytes));
    } catch (error) {
      this.clear();
      throw error;
    }
    let pending!: PendingFragment;
    const fragment: AttachmentTransferPacketQueueFragment = {
      bytes,
      direction: this.direction,
      messageId: message.messageId,
      final: message.offsetBytes + payloadBytes === message.bytes.byteLength,
      commit: () => this.commit(pending),
    };
    pending = { message, payloadBytes, fragment, committed: false };
    this.pending = pending;
    return fragment;
  }

  /** Releases every retained source buffer and closes this channel generation. */
  clear(): void {
    const messages = this.messages.splice(0);
    this.closed = true;
    this.pending = null;
    this.retainedBytes = 0;
    let failure: unknown;
    for (const message of messages) {
      try {
        this.release(message);
      } catch (error) {
        failure ??= error;
      }
    }
    if (failure !== undefined) throw failure;
  }

  /** Releases this generation and starts a fresh message-ID sequence. */
  reset(): void {
    this.clear();
    this.nextMessageId = 1;
    this.closed = false;
  }

  private commit(pending: PendingFragment): void {
    if (pending.committed) return;
    pending.committed = true;
    if (this.pending !== pending) return;
    this.pending = null;
    const message = pending.message;
    message.offsetBytes += pending.payloadBytes;
    if (message.offsetBytes !== message.bytes.byteLength) return;
    if (this.messages[0] !== message) throw new AttachmentTransferPacketError("fragment-order");
    this.messages.shift();
    this.retainedBytes -= message.bytes.byteLength;
    this.release(message);
  }

  private release(message: QueuedMessage): void {
    if (message.released) return;
    message.released = true;
    this.quota.release(this.direction, message.bytes.byteLength);
  }
}
