// Attachment-specific WebRTC outer framing for direct uploads.
// Browser and worker use it on separate attachment data channels; it shares no
// terminal lane, magic, packet ownership, or framing state.

import {
  ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES,
  ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES,
  ATTACHMENT_TRANSFER_PACKET_MAGIC,
  ATTACHMENT_TRANSFER_PACKET_MAX_BYTES,
  ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES,
  ATTACHMENT_TRANSFER_PACKET_STALL_MS,
  ATTACHMENT_TRANSFER_PACKET_VERSION,
} from "./attachment-transfer.ts";

export {
  ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES,
  ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES,
  ATTACHMENT_TRANSFER_PACKET_MAGIC,
  ATTACHMENT_TRANSFER_PACKET_MAX_BYTES,
  ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES,
  ATTACHMENT_TRANSFER_PACKET_STALL_MS,
  ATTACHMENT_TRANSFER_PACKET_VERSION,
} from "./attachment-transfer.ts";

export type AttachmentTransferPacketDirection = "incoming" | "outgoing";

export type AttachmentTransferPacketErrorCode =
  | "packet-size"
  | "packet-magic"
  | "packet-version"
  | "packet-header"
  | "message-id"
  | "message-id-wrap"
  | "message-size"
  | "fragment-order"
  | "fragment-stalled"
  | "quota"
  | "allocation"
  | "clock"
  | "closed";

export class AttachmentTransferPacketError extends Error {
  readonly code: AttachmentTransferPacketErrorCode;

  constructor(code: AttachmentTransferPacketErrorCode) {
    super(`attachment transfer packet rejected: ${code}`);
    this.name = "AttachmentTransferPacketError";
    this.code = code;
  }
}

export interface AttachmentTransferPacketHeader {
  readonly messageId: number;
  readonly totalBytes: number;
  readonly offsetBytes: number;
}

export interface AttachmentTransferPacket extends AttachmentTransferPacketHeader {
  /** A borrowed view of one outer packet. Assemblers return an owned logical buffer. */
  readonly payload: Uint8Array;
}

/** Retained-byte accounting remains independent for incoming and outgoing packets. */
export interface AttachmentTransferPacketQuota {
  reserve(direction: AttachmentTransferPacketDirection, bytes: number): boolean;
  release(direction: AttachmentTransferPacketDirection, bytes: number): void;
}

export {
  AttachmentTransferPacketQueue,
  type AttachmentTransferPacketQueueFragment,
} from "./attachment-transfer-packet-queue.ts";

interface PartialMessage {
  readonly messageId: number;
  readonly totalBytes: number;
  readonly bytes: Uint8Array;
  nextOffset: number;
  lastFragmentAtMs: number;
}

/** Encodes one complete attachment packet with its magic and version header. */
export function encodeAttachmentTransferPacket(
  header: AttachmentTransferPacketHeader,
  payload: Uint8Array,
): Uint8Array {
  assertPacketFields(header, payload.byteLength);
  const packet = new Uint8Array(ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES + payload.byteLength);
  const view = new DataView(packet.buffer, packet.byteOffset, ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES);
  view.setUint32(0, ATTACHMENT_TRANSFER_PACKET_MAGIC, true);
  view.setUint32(4, ATTACHMENT_TRANSFER_PACKET_VERSION, true);
  view.setUint32(8, header.messageId, true);
  view.setUint32(12, header.totalBytes, true);
  view.setUint32(16, header.offsetBytes, true);
  packet.set(payload, ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES);
  return packet;
}

/** Parses one bounded outer packet without allocating a logical-frame buffer. */
export function parseAttachmentTransferPacket(packet: Uint8Array): AttachmentTransferPacket {
  if (
    packet.byteLength <= ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES
    || packet.byteLength > ATTACHMENT_TRANSFER_PACKET_MAX_BYTES
  ) {
    throw new AttachmentTransferPacketError("packet-size");
  }
  const view = new DataView(packet.buffer, packet.byteOffset, ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES);
  if (view.getUint32(0, true) !== ATTACHMENT_TRANSFER_PACKET_MAGIC) {
    throw new AttachmentTransferPacketError("packet-magic");
  }
  if (view.getUint32(4, true) !== ATTACHMENT_TRANSFER_PACKET_VERSION) {
    throw new AttachmentTransferPacketError("packet-version");
  }
  const header = {
    messageId: view.getUint32(8, true),
    totalBytes: view.getUint32(12, true),
    offsetBytes: view.getUint32(16, true),
  };
  const payload = packet.subarray(ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES);
  assertPacketFields(header, payload.byteLength);
  return { ...header, payload };
}

/** One ordered attachment channel's single-message reassembly state. */
export class AttachmentTransferPacketAssembler {
  private partial: PartialMessage | null = null;
  private lastCompletedMessageId = 0;
  private closed = false;

  constructor(
    readonly direction: AttachmentTransferPacketDirection,
    private readonly quota: AttachmentTransferPacketQuota,
    private readonly now: () => number = monotonicNowMs,
  ) {}

  get hasPartialMessage(): boolean {
    return this.partial !== null;
  }

  get isClosed(): boolean {
    return this.closed;
  }

  get lastMessageId(): number {
    return this.lastCompletedMessageId;
  }

  /** Returns an owned logical frame once every ordered fragment has arrived. */
  push(packetBytes: Uint8Array, nowMs = this.now()): Uint8Array | null {
    if (this.closed) throw new AttachmentTransferPacketError("closed");
    try {
      assertMonotonicTime(nowMs);
      if (this.partial && nowMs - this.partial.lastFragmentAtMs >= ATTACHMENT_TRANSFER_PACKET_STALL_MS) {
        throw new AttachmentTransferPacketError("fragment-stalled");
      }
      return this.accept(parseAttachmentTransferPacket(packetBytes), nowMs);
    } catch (error) {
      this.closeInternal();
      throw error;
    }
  }

  /** Releases a stalled partial buffer so callers can retire the peer. */
  expire(nowMs = this.now()): boolean {
    try {
      assertMonotonicTime(nowMs);
      if (!this.partial || nowMs - this.partial.lastFragmentAtMs < ATTACHMENT_TRANSFER_PACKET_STALL_MS) {
        return false;
      }
      this.closeInternal();
      return true;
    } catch (error) {
      this.closeInternal();
      throw error;
    }
  }

  /** Releases retained state and starts a fresh ordered message sequence. */
  reset(): void {
    this.releasePartial();
    this.lastCompletedMessageId = 0;
    this.closed = false;
  }

  private accept(packet: AttachmentTransferPacket, nowMs: number): Uint8Array | null {
    if (!this.partial) return this.begin(packet, nowMs);
    if (
      packet.messageId !== this.partial.messageId
      || packet.totalBytes !== this.partial.totalBytes
      || packet.offsetBytes !== this.partial.nextOffset
    ) {
      throw new AttachmentTransferPacketError("fragment-order");
    }
    this.partial.bytes.set(packet.payload, packet.offsetBytes);
    this.partial.nextOffset += packet.payload.byteLength;
    this.partial.lastFragmentAtMs = nowMs;
    if (this.partial.nextOffset !== this.partial.totalBytes) return null;
    return this.completePartial();
  }

  private begin(packet: AttachmentTransferPacket, nowMs: number): Uint8Array | null {
    if (this.lastCompletedMessageId === 0xffff_ffff) {
      throw new AttachmentTransferPacketError("message-id-wrap");
    }
    if (packet.messageId !== this.lastCompletedMessageId + 1 || packet.offsetBytes !== 0) {
      throw new AttachmentTransferPacketError("message-id");
    }
    if (packet.payload.byteLength === packet.totalBytes) return this.copySinglePacket(packet);
    if (!this.quota.reserve(this.direction, packet.totalBytes)) {
      throw new AttachmentTransferPacketError("quota");
    }
    try {
      const bytes = new Uint8Array(packet.totalBytes);
      bytes.set(packet.payload);
      this.partial = {
        messageId: packet.messageId,
        totalBytes: packet.totalBytes,
        bytes,
        nextOffset: packet.payload.byteLength,
        lastFragmentAtMs: nowMs,
      };
      return null;
    } catch {
      this.quota.release(this.direction, packet.totalBytes);
      throw new AttachmentTransferPacketError("allocation");
    }
  }

  private copySinglePacket(packet: AttachmentTransferPacket): Uint8Array {
    if (!this.quota.reserve(this.direction, packet.totalBytes)) {
      throw new AttachmentTransferPacketError("quota");
    }
    try {
      const bytes = new Uint8Array(packet.payload);
      this.lastCompletedMessageId = packet.messageId;
      return bytes;
    } catch {
      throw new AttachmentTransferPacketError("allocation");
    } finally {
      this.quota.release(this.direction, packet.totalBytes);
    }
  }

  private completePartial(): Uint8Array {
    const partial = this.partial;
    if (!partial) throw new AttachmentTransferPacketError("fragment-order");
    this.partial = null;
    this.lastCompletedMessageId = partial.messageId;
    this.quota.release(this.direction, partial.totalBytes);
    return partial.bytes;
  }

  private releasePartial(): void {
    const partial = this.partial;
    this.partial = null;
    if (partial) this.quota.release(this.direction, partial.totalBytes);
  }

  private closeInternal(): void {
    this.closed = true;
    this.releasePartial();
  }
}

function assertPacketFields(header: AttachmentTransferPacketHeader, payloadBytes: number): void {
  if (!Number.isInteger(header.messageId) || header.messageId < 1 || header.messageId > 0xffff_ffff) {
    throw new AttachmentTransferPacketError("message-id");
  }
  if (
    !Number.isInteger(header.totalBytes)
    || header.totalBytes < 1
    || header.totalBytes > ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES
  ) {
    throw new AttachmentTransferPacketError("message-size");
  }
  if (
    !Number.isInteger(header.offsetBytes)
    || header.offsetBytes < 0
    || header.offsetBytes >= header.totalBytes
    || payloadBytes < 1
    || payloadBytes > ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES
    || header.offsetBytes + payloadBytes > header.totalBytes
  ) {
    throw new AttachmentTransferPacketError("packet-header");
  }
}

function monotonicNowMs(): number {
  const now = globalThis.performance?.now();
  assertMonotonicTime(now);
  return now;
}

function assertMonotonicTime(nowMs: number | undefined): asserts nowMs is number {
  if (typeof nowMs !== "number" || !Number.isFinite(nowMs) || nowMs < 0) {
    throw new AttachmentTransferPacketError("clock");
  }
}
