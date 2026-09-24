// Tests the outer binary framing and retained-byte ownership for terminal peers.
// Packets must fail closed on ordering or quota faults so one slow/malformed peer
// cannot splice a message, reuse an ID, or retain application memory indefinitely.

import { describe, expect, test } from "bun:test";
import {
  encodeTerminalPeerPacket,
  parseTerminalPeerPacket,
  TerminalPeerPacketAssembler,
  TerminalPeerPacketError,
  TerminalPeerPacketQueue,
  TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES,
  TERMINAL_PEER_PACKET_MAGIC,
  TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
  TERMINAL_PEER_PACKET_STALL_MS,
  type TerminalPeerPacketQuota,
} from "../src/terminal-peer-packets.ts";

class RecordingQuota implements TerminalPeerPacketQuota {
  reserved = 0;
  reserveCalls: number[] = [];
  releaseCalls: number[] = [];

  constructor(private readonly limit = Number.POSITIVE_INFINITY) {}

  reserve(bytes: number): boolean {
    this.reserveCalls.push(bytes);
    if (this.reserved + bytes > this.limit) return false;
    this.reserved += bytes;
    return true;
  }

  release(bytes: number): void {
    this.releaseCalls.push(bytes);
    this.reserved -= bytes;
  }
}

function packet(messageId: number, totalBytes: number, offsetBytes: number, payload: Uint8Array): Uint8Array {
  return encodeTerminalPeerPacket("control", { messageId, totalBytes, offsetBytes }, payload);
}

function packetCode(action: () => unknown): string {
  try {
    action();
  } catch (error) {
    expect(error).toBeInstanceOf(TerminalPeerPacketError);
    return (error as TerminalPeerPacketError).code;
  }
  throw new Error("expected packet rejection");
}

function forceQueueNextId(queue: TerminalPeerPacketQueue, messageId: number): void {
  // Exhausting the 32-bit sequence would make this boundary test impractical;
  // production can reach this state only by sending that many ordered messages.
  Reflect.set(queue, "nextMessageId", messageId);
}

function forceAssemblerLastId(assembler: TerminalPeerPacketAssembler, messageId: number): void {
  Reflect.set(assembler, "lastCompletedMessageId", messageId);
}

describe("terminal peer packet encoding and reassembly", () => {
  test("writes the exact mandatory little-endian header", () => {
    const encoded = encodeTerminalPeerPacket("control", {
      messageId: 0x0102_0304,
      totalBytes: 3,
      offsetBytes: 0,
    }, new Uint8Array([9, 8, 7]));

    expect(Array.from(encoded.slice(0, 16))).toEqual([
      0x52, 0x54, 0x50, 0x31,
      0x04, 0x03, 0x02, 0x01,
      0x03, 0x00, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00,
    ]);
    expect(new DataView(encoded.buffer).getUint32(0, true)).toBe(TERMINAL_PEER_PACKET_MAGIC);
    expect(parseTerminalPeerPacket("control", encoded)).toMatchObject({
      messageId: 0x0102_0304,
      totalBytes: 3,
      offsetBytes: 0,
      payload: new Uint8Array([9, 8, 7]),
    });
  });

  test("reassembles fragments in order and releases the complete retained reservation", () => {
    const quota = new RecordingQuota();
    const assembler = new TerminalPeerPacketAssembler("control", quota, () => 0);
    const source = Uint8Array.from({ length: TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 5 }, (_, index) => index % 251);

    expect(assembler.push(packet(1, source.byteLength, 0, source.slice(0, TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES)), 1))
      .toBeNull();
    expect(quota.reserved).toBe(source.byteLength);
    expect(assembler.push(packet(1, source.byteLength, TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES, source.slice(TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES)), 2))
      .toEqual(source);
    expect(quota.reserved).toBe(0);
    expect(quota.releaseCalls).toEqual([source.byteLength]);
  });

  test("requires consecutive nonzero IDs and closes on reuse or wrap", () => {
    const quota = new RecordingQuota();
    const assembler = new TerminalPeerPacketAssembler("control", quota);
    expect(assembler.push(packet(1, 1, 0, new Uint8Array([1])), 1)).toEqual(new Uint8Array([1]));
    expect(packetCode(() => assembler.push(packet(1, 1, 0, new Uint8Array([2])), 2))).toBe("message-id");
    expect(assembler.isClosed).toBe(true);
    expect(packetCode(() => packet(0, 1, 0, new Uint8Array([1])))).toBe("message-id");

    const wrapped = new TerminalPeerPacketAssembler("control", new RecordingQuota());
    forceAssemblerLastId(wrapped, 0xffff_ffff);
    expect(packetCode(() => wrapped.push(packet(1, 1, 0, new Uint8Array([1])), 1))).toBe("message-id-wrap");
  });

  test("rejects empty, malformed, and lane-overlimit packets before allocating a logical buffer", () => {
    expect(packetCode(() => parseTerminalPeerPacket("control", new Uint8Array(16)))).toBe("packet-size");
    expect(packetCode(() => packet(1, 2, 1, new Uint8Array([1, 2])))).toBe("packet-header");
    expect(packetCode(() => packet(1, 0, 0, new Uint8Array([1])))).toBe("message-size");
    expect(packetCode(() => packet(
      1,
      TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES.control + 1,
      0,
      new Uint8Array([1]),
    ))).toBe("message-size");
  });

  test("refuses quota admission and releases a stalled partial message exactly once", () => {
    const refused = new TerminalPeerPacketAssembler("control", new RecordingQuota(1));
    expect(packetCode(() => refused.push(packet(1, 2, 0, new Uint8Array([1])), 1))).toBe("quota");

    const quota = new RecordingQuota();
    const assembler = new TerminalPeerPacketAssembler("control", quota);
    expect(assembler.push(packet(1, 2, 0, new Uint8Array([1])), 100)).toBeNull();
    expect(assembler.expire(100 + TERMINAL_PEER_PACKET_STALL_MS - 1)).toBe(false);
    expect(assembler.expire(100 + TERMINAL_PEER_PACKET_STALL_MS)).toBe(true);
    expect(assembler.isClosed).toBe(true);
    expect(quota.reserved).toBe(0);
    expect(quota.releaseCalls).toEqual([2]);
  });
});

describe("terminal peer packet FIFO queue", () => {
  test("retains whole messages FIFO while materializing and committing one fragment at a time", () => {
    const quota = new RecordingQuota();
    const queue = new TerminalPeerPacketQueue("control", quota);
    const first = Uint8Array.from({ length: TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 1 }, () => 1);
    const second = new Uint8Array([2, 3]);

    expect(queue.enqueue(first)).toBe(true);
    expect(queue.enqueue(second)).toBe(true);
    expect(queue.queuedBytes).toBe(first.byteLength + second.byteLength);
    expect(queue.messageCount).toBe(2);

    const firstFragment = queue.nextFragment()!;
    expect(queue.nextFragment()).toBe(firstFragment);
    expect(firstFragment.bytes.byteLength).toBe(TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 16);
    expect(parseTerminalPeerPacket("control", firstFragment.bytes)).toMatchObject({
      messageId: 1, totalBytes: first.byteLength, offsetBytes: 0,
    });
    firstFragment.commit();
    firstFragment.commit();

    const finalFirstFragment = queue.nextFragment()!;
    expect(parseTerminalPeerPacket("control", finalFirstFragment.bytes)).toMatchObject({
      messageId: 1,
      totalBytes: first.byteLength,
      offsetBytes: TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
    });
    expect(finalFirstFragment.final).toBe(true);
    finalFirstFragment.commit();

    const secondFragment = queue.nextFragment()!;
    expect(parseTerminalPeerPacket("control", secondFragment.bytes)).toMatchObject({
      messageId: 2, totalBytes: second.byteLength, offsetBytes: 0,
    });
    secondFragment.commit();
    expect(queue.nextFragment()).toBeNull();
    expect(queue.queuedBytes).toBe(0);
    expect(quota.reserved).toBe(0);
    expect(quota.releaseCalls).toEqual([first.byteLength, second.byteLength]);
  });

  test("makes clear terminal and requires reset before a fresh ID sequence", () => {
    const refused = new TerminalPeerPacketQueue("control", new RecordingQuota(1));
    expect(refused.enqueue(new Uint8Array([1, 2]))).toBe(false);
    expect(refused.messageCount).toBe(0);

    const quota = new RecordingQuota();
    const queue = new TerminalPeerPacketQueue("control", quota);
    expect(queue.enqueue(new Uint8Array([1]))).toBe(true);
    queue.nextFragment();
    queue.clear();
    queue.clear();
    expect(queue.isClosed).toBe(true);
    expect(quota.releaseCalls).toEqual([1]);
    expect(packetCode(() => queue.enqueue(new Uint8Array([2])))).toBe("closed");

    queue.reset();
    expect(queue.enqueue(new Uint8Array([3]))).toBe(true);
    expect(parseTerminalPeerPacket("control", queue.nextFragment()!.bytes).messageId).toBe(1);
  });

  test("fails closed rather than reusing an exhausted 32-bit queue sequence", () => {
    const queue = new TerminalPeerPacketQueue("control", new RecordingQuota());
    forceQueueNextId(queue, 0xffff_ffff);
    expect(queue.enqueue(new Uint8Array([1]))).toBe(true);
    queue.nextFragment()!.commit();
    expect(packetCode(() => queue.enqueue(new Uint8Array([2])))).toBe("message-id-wrap");
  });
});
