// Shared outer framing for every negotiated terminal-peer data channel.
// The assembler validates a packet before reserving or allocating its logical
// message buffer; queue ownership lives in terminal-peer-packet-queue.ts.

import {
  TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES,
  TERMINAL_PEER_PACKET_HEADER_BYTES,
  TERMINAL_PEER_PACKET_MAGIC,
  TERMINAL_PEER_PACKET_MAX_BYTES,
  TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
  TERMINAL_PEER_PACKET_STALL_MS,
  type TerminalPeerPacketLane,
} from "./terminal-peer.ts";

export {
  TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES,
  TERMINAL_PEER_PACKET_HEADER_BYTES,
  TERMINAL_PEER_PACKET_MAGIC,
  TERMINAL_PEER_PACKET_MAX_BYTES,
  TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
  TERMINAL_PEER_PACKET_STALL_MS,
  type TerminalPeerPacketLane,
};

export type TerminalPeerPacketErrorCode =
  | "packet-size"
  | "packet-magic"
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

export class TerminalPeerPacketError extends Error {
  readonly code: TerminalPeerPacketErrorCode;

  constructor(code: TerminalPeerPacketErrorCode) {
    super(`terminal peer packet rejected: ${code}`);
    this.name = "TerminalPeerPacketError";
    this.code = code;
  }
}

export interface TerminalPeerPacketHeader {
  readonly messageId: number;
  readonly totalBytes: number;
  readonly offsetBytes: number;
}

export interface TerminalPeerPacket extends TerminalPeerPacketHeader {
  /** A borrowed view of the packet source. The assembler makes an owned copy. */
  readonly payload: Uint8Array;
}

/** Retained-byte accounting for one direction and peer. */
export interface TerminalPeerPacketQuota {
  /** Reserves bytes before allocation or ownership transfer. */
  reserve(bytes: number): boolean;
  /** Releases a reservation previously accepted by reserve. */
  release(bytes: number): void;
}

interface PartialMessage {
  readonly messageId: number;
  readonly totalBytes: number;
  readonly bytes: Uint8Array;
  nextOffset: number;
  lastFragmentAtMs: number;
}

/** Encodes one complete outer packet, including its mandatory 16-byte header. */
export function encodeTerminalPeerPacket(
  lane: TerminalPeerPacketLane,
  header: TerminalPeerPacketHeader,
  payload: Uint8Array,
): Uint8Array {
  assertPacketFields(lane, header, payload.byteLength);
  const packet = new Uint8Array(TERMINAL_PEER_PACKET_HEADER_BYTES + payload.byteLength);
  const view = new DataView(packet.buffer, packet.byteOffset, TERMINAL_PEER_PACKET_HEADER_BYTES);
  view.setUint32(0, TERMINAL_PEER_PACKET_MAGIC, true);
  view.setUint32(4, header.messageId, true);
  view.setUint32(8, header.totalBytes, true);
  view.setUint32(12, header.offsetBytes, true);
  packet.set(payload, TERMINAL_PEER_PACKET_HEADER_BYTES);
  return packet;
}

/** Parses and bounds a raw packet without allocating a logical message buffer. */
export function parseTerminalPeerPacket(
  lane: TerminalPeerPacketLane,
  packet: Uint8Array,
): TerminalPeerPacket {
  if (
    packet.byteLength <= TERMINAL_PEER_PACKET_HEADER_BYTES
    || packet.byteLength > TERMINAL_PEER_PACKET_MAX_BYTES
  ) {
    throw new TerminalPeerPacketError("packet-size");
  }
  const view = new DataView(packet.buffer, packet.byteOffset, TERMINAL_PEER_PACKET_HEADER_BYTES);
  if (view.getUint32(0, true) !== TERMINAL_PEER_PACKET_MAGIC) {
    throw new TerminalPeerPacketError("packet-magic");
  }
  const header = {
    messageId: view.getUint32(4, true),
    totalBytes: view.getUint32(8, true),
    offsetBytes: view.getUint32(12, true),
  };
  const payload = packet.subarray(TERMINAL_PEER_PACKET_HEADER_BYTES);
  assertPacketFields(lane, header, payload.byteLength);
  return { ...header, payload };
}

/** One ordered channel's consecutive single-message reassembly state. */
export class TerminalPeerPacketAssembler {
  private partial: PartialMessage | null = null;
  private lastCompletedMessageId = 0;
  private closed = false;


  constructor(
    private readonly lane: TerminalPeerPacketLane,
    private readonly quota: TerminalPeerPacketQuota,
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

  /** Returns a fully owned logical payload, or null until its final fragment arrives. */
  push(packetBytes: Uint8Array, nowMs = this.now()): Uint8Array | null {
    if (this.closed) throw new TerminalPeerPacketError("closed");
    try {
      assertMonotonicTime(nowMs);
      if (this.partial && nowMs - this.partial.lastFragmentAtMs >= TERMINAL_PEER_PACKET_STALL_MS) {
        throw new TerminalPeerPacketError("fragment-stalled");
      }
      return this.accept(parseTerminalPeerPacket(this.lane, packetBytes), nowMs);
    } catch (error) {
      this.closeInternal();
      throw error;
    }
  }

  /** Releases a stalled partial buffer. Callers retire the peer when this is true. */
  expire(nowMs = this.now()): boolean {
    try {
      assertMonotonicTime(nowMs);
      if (!this.partial || nowMs - this.partial.lastFragmentAtMs < TERMINAL_PEER_PACKET_STALL_MS) {
        return false;
      }
      this.closeInternal();
      return true;
    } catch (error) {
      this.closeInternal();
      throw error;
    }
  }

  /** Clears this generation's ID sequence and releases any retained partial buffer. */
  reset(): void {
    this.releasePartial();
    this.lastCompletedMessageId = 0;
    this.closed = false;
  }

  private accept(packet: TerminalPeerPacket, nowMs: number): Uint8Array | null {
    if (!this.partial) return this.begin(packet, nowMs);
    if (
      packet.messageId !== this.partial.messageId
      || packet.totalBytes !== this.partial.totalBytes
      || packet.offsetBytes !== this.partial.nextOffset
    ) {
      throw new TerminalPeerPacketError("fragment-order");
    }
    this.partial.bytes.set(packet.payload, packet.offsetBytes);
    this.partial.nextOffset += packet.payload.byteLength;
    this.partial.lastFragmentAtMs = nowMs;
    if (this.partial.nextOffset !== this.partial.totalBytes) return null;
    return this.completePartial();
  }

  private begin(packet: TerminalPeerPacket, nowMs: number): Uint8Array | null {
    if (this.lastCompletedMessageId === 0xffff_ffff) {
      throw new TerminalPeerPacketError("message-id-wrap");
    }
    if (packet.messageId !== this.lastCompletedMessageId + 1 || packet.offsetBytes !== 0) {
      throw new TerminalPeerPacketError("message-id");
    }

    if (packet.payload.byteLength === packet.totalBytes) {
      return this.copySinglePacket(packet);
    }
    if (!this.quota.reserve(packet.totalBytes)) throw new TerminalPeerPacketError("quota");
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
      this.quota.release(packet.totalBytes);
      throw new TerminalPeerPacketError("allocation");
    }
  }

  private copySinglePacket(packet: TerminalPeerPacket): Uint8Array {
    if (!this.quota.reserve(packet.totalBytes)) throw new TerminalPeerPacketError("quota");
    try {
      const bytes = new Uint8Array(packet.payload);
      this.lastCompletedMessageId = packet.messageId;
      return bytes;
    } catch {
      throw new TerminalPeerPacketError("allocation");
    } finally {
      this.quota.release(packet.totalBytes);
    }
  }

  private completePartial(): Uint8Array {
    const partial = this.partial;
    if (!partial) throw new TerminalPeerPacketError("fragment-order");
    this.partial = null;
    this.lastCompletedMessageId = partial.messageId;
    this.quota.release(partial.totalBytes);
    return partial.bytes;
  }

  private releasePartial(): void {
    const partial = this.partial;
    this.partial = null;
    if (partial) this.quota.release(partial.totalBytes);
  }

  private closeInternal(): void {
    this.closed = true;
    this.releasePartial();
  }
}

function assertPacketFields(
  lane: TerminalPeerPacketLane,
  header: TerminalPeerPacketHeader,
  payloadBytes: number,
): void {
  if (!Number.isInteger(header.messageId) || header.messageId < 1 || header.messageId > 0xffff_ffff) {
    throw new TerminalPeerPacketError("message-id");
  }
  const logicalLimit = TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES[lane];
  if (
    !Number.isInteger(header.totalBytes)
    || header.totalBytes < 1
    || !Number.isInteger(logicalLimit)
    || header.totalBytes > logicalLimit
  ) {
    throw new TerminalPeerPacketError("message-size");
  }
  if (
    !Number.isInteger(header.offsetBytes)
    || header.offsetBytes < 0
    || header.offsetBytes >= header.totalBytes
    || payloadBytes < 1
    || payloadBytes > TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES
    || header.offsetBytes + payloadBytes > header.totalBytes
  ) {
    throw new TerminalPeerPacketError("packet-header");
  }
}

function monotonicNowMs(): number {
  const now = globalThis.performance?.now();
  assertMonotonicTime(now);
  return now;
}

function assertMonotonicTime(nowMs: number): void {
  if (!Number.isFinite(nowMs) || nowMs < 0) throw new TerminalPeerPacketError("clock");
}

export {
  TerminalPeerPacketQueue,
  type TerminalPeerPacketQueueFragment,
} from "./terminal-peer-packet-queue.ts";
