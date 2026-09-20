// FIFO ownership queue for one terminal-peer data-channel lane.
// It retains complete logical buffers, produces one framed packet on demand,
// and releases each retained-byte reservation exactly once on drain or reset.

import {
  TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES,
  TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
  type TerminalPeerPacketLane,
} from "./terminal-peer.ts";
import {
  encodeTerminalPeerPacket,
  TerminalPeerPacketError,
  type TerminalPeerPacketQuota,
} from "./terminal-peer-packets.ts";

interface QueuedMessage {
  readonly messageId: number;
  readonly bytes: Uint8Array;
  offsetBytes: number;
  released: boolean;
}

interface PendingFragment {
  readonly message: QueuedMessage;
  readonly payloadBytes: number;
  readonly fragment: TerminalPeerPacketQueueFragment;
  committed: boolean;
}

/** A materialized packet remains owned by its queue until commit confirms send acceptance. */
export interface TerminalPeerPacketQueueFragment {
  readonly bytes: Uint8Array;
  readonly lane: TerminalPeerPacketLane;
  readonly messageId: number;
  readonly final: boolean;
  /** Advances this one packet after the native or browser send accepted it. */
  commit(): void;
}

/** FIFO source-buffer ownership for one ordered data-channel lane. */
export class TerminalPeerPacketQueue {
  private readonly messages: QueuedMessage[] = [];
  private pending: PendingFragment | null = null;
  private nextMessageId = 1;
  private retainedBytes = 0;
  private closed = false;


  constructor(
    readonly lane: TerminalPeerPacketLane,
    private readonly quota: TerminalPeerPacketQuota,
  ) {}

  /** Complete logical bytes retained by this queue, including an active message. */
  get queuedBytes(): number {
    return this.retainedBytes;
  }

  get messageCount(): number {
    return this.messages.length;
  }

  get isClosed(): boolean {
    return this.closed;
  }


  /**
   * Transfers ownership of bytes without copying or pre-fragmenting them.
   * False means admission refused before ownership transferred.
   */
  enqueue(bytes: Uint8Array): boolean {
    if (this.closed) throw new TerminalPeerPacketError("closed");
    const limit = TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES[this.lane];
    if (!Number.isInteger(limit) || bytes.byteLength < 1 || bytes.byteLength > limit) {
      throw new TerminalPeerPacketError("message-size");
    }
    if (this.nextMessageId === 0) throw new TerminalPeerPacketError("message-id-wrap");
    if (!this.quota.reserve(bytes.byteLength)) return false;

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
      this.quota.release(bytes.byteLength);
      throw new TerminalPeerPacketError("allocation");
    }
  }

  /** Materializes at most one <=16 KiB packet and caches it until commit or reset. */
  nextFragment(): TerminalPeerPacketQueueFragment | null {
    if (this.pending) return this.pending.fragment;
    const message = this.messages[0];
    if (!message) return null;

    const payloadBytes = Math.min(
      TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
      message.bytes.byteLength - message.offsetBytes,
    );
    let packet: Uint8Array;
    try {
      packet = encodeTerminalPeerPacket(
        this.lane,
        {
          messageId: message.messageId,
          totalBytes: message.bytes.byteLength,
          offsetBytes: message.offsetBytes,
        },
        message.bytes.subarray(message.offsetBytes, message.offsetBytes + payloadBytes),
      );
    } catch (error) {
      this.clear();
      throw error;
    }
    let pending!: PendingFragment;
    const fragment: TerminalPeerPacketQueueFragment = {
      bytes: packet,
      lane: this.lane,
      messageId: message.messageId,
      final: message.offsetBytes + payloadBytes === message.bytes.byteLength,
      commit: () => this.commit(pending),
    };
    pending = { message, payloadBytes, fragment, committed: false };
    this.pending = pending;
    return fragment;
  }

  /** Releases all owned buffers and closes this channel generation. */
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

  /** Releases this generation and opens a fresh message-ID sequence. */
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
    if (this.messages[0] !== message) throw new TerminalPeerPacketError("fragment-order");
    this.messages.shift();
    this.retainedBytes -= message.bytes.byteLength;
    this.release(message);
  }

  private release(message: QueuedMessage): void {
    if (message.released) return;
    message.released = true;
    this.quota.release(message.bytes.byteLength);
  }
}
