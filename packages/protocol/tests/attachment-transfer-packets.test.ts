// Tests attachment-specific WebRTC framing, retained-byte ownership, and order.
// Direct upload packets must reject terminal framing, malformed order, stalled
// fragments, and quota overreach before one peer can retain or replay bytes.

import { describe, expect, test } from "bun:test";
import {
  AttachmentTransferPacketAssembler,
  AttachmentTransferPacketError,
  AttachmentTransferPacketQueue,
  ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES,
  ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES,
  ATTACHMENT_TRANSFER_PACKET_MAGIC,
  ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES,
  ATTACHMENT_TRANSFER_PACKET_STALL_MS,
  ATTACHMENT_TRANSFER_PACKET_VERSION,
  encodeAttachmentTransferPacket,
  parseAttachmentTransferPacket,
  type AttachmentTransferPacketDirection,
  type AttachmentTransferPacketQuota,
} from "../src/attachment-transfer-packets.ts";

class RecordingQuota implements AttachmentTransferPacketQuota {
  readonly reserved: Record<AttachmentTransferPacketDirection, number> = { incoming: 0, outgoing: 0 };
  readonly reserveCalls: Array<{ direction: AttachmentTransferPacketDirection; bytes: number }> = [];
  readonly releaseCalls: Array<{ direction: AttachmentTransferPacketDirection; bytes: number }> = [];

  constructor(private readonly limit = Number.POSITIVE_INFINITY) {}

  reserve(direction: AttachmentTransferPacketDirection, bytes: number): boolean {
    this.reserveCalls.push({ direction, bytes });
    if (this.reserved[direction] + bytes > this.limit) return false;
    this.reserved[direction] += bytes;
    return true;
  }

  release(direction: AttachmentTransferPacketDirection, bytes: number): void {
    this.releaseCalls.push({ direction, bytes });
    this.reserved[direction] -= bytes;
  }
}

function encodedPacket(
  messageId: number,
  totalBytes: number,
  offsetBytes: number,
  payload: Uint8Array,
): Uint8Array {
  return encodeAttachmentTransferPacket({ messageId, totalBytes, offsetBytes }, payload);
}

function packetCode(action: () => unknown): string {
  try {
    action();
  } catch (error) {
    expect(error).toBeInstanceOf(AttachmentTransferPacketError);
    return (error as AttachmentTransferPacketError).code;
  }
  throw new Error("expected attachment packet rejection");
}

describe("attachment transfer packet framing", () => {
  test("writes an attachment-only little-endian magic and version header", () => {
    const encoded = encodedPacket(0x0102_0304, 3, 0, new Uint8Array([9, 8, 7]));
    expect([...encoded.slice(0, ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES)]).toEqual([
      0x41, 0x54, 0x50, 0x31,
      0x01, 0x00, 0x00, 0x00,
      0x04, 0x03, 0x02, 0x01,
      0x03, 0x00, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00,
    ]);
    expect(new DataView(encoded.buffer).getUint32(0, true)).toBe(ATTACHMENT_TRANSFER_PACKET_MAGIC);
    expect(new DataView(encoded.buffer).getUint32(4, true)).toBe(ATTACHMENT_TRANSFER_PACKET_VERSION);
    expect(parseAttachmentTransferPacket(encoded)).toMatchObject({
      messageId: 0x0102_0304,
      totalBytes: 3,
      offsetBytes: 0,
      payload: new Uint8Array([9, 8, 7]),
    });
  });

  test("reassembles only consecutive fragments under the incoming quota", () => {
    const quota = new RecordingQuota();
    const assembler = new AttachmentTransferPacketAssembler("incoming", quota, () => 0);
    const source = Uint8Array.from(
      { length: ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES + 5 },
      (_, index) => index % 251,
    );
    expect(assembler.push(
      encodedPacket(1, source.byteLength, 0, source.slice(0, ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES)),
      1,
    )).toBeNull();
    expect(quota.reserved.incoming).toBe(source.byteLength);
    expect(assembler.push(
      encodedPacket(1, source.byteLength, ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES, source.slice(ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES)),
      2,
    )).toEqual(source);
    expect(quota.reserved.incoming).toBe(0);
    expect(quota.releaseCalls).toEqual([{ direction: "incoming", bytes: source.byteLength }]);
  });

  test("rejects terminal magic, wrong versions, malformed order, and logical frames above one MiB", () => {
    const terminalMagic = encodedPacket(1, 1, 0, new Uint8Array([1]));
    new DataView(terminalMagic.buffer).setUint32(0, 0x3150_5452, true);
    expect(packetCode(() => parseAttachmentTransferPacket(terminalMagic))).toBe("packet-magic");

    const wrongVersion = encodedPacket(1, 1, 0, new Uint8Array([1]));
    new DataView(wrongVersion.buffer).setUint32(4, ATTACHMENT_TRANSFER_PACKET_VERSION + 1, true);
    expect(packetCode(() => parseAttachmentTransferPacket(wrongVersion))).toBe("packet-version");
    expect(packetCode(() => encodedPacket(
      1,
      ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES + 1,
      0,
      new Uint8Array([1]),
    ))).toBe("message-size");

    const assembler = new AttachmentTransferPacketAssembler("incoming", new RecordingQuota());
    expect(assembler.push(encodedPacket(1, 2, 0, new Uint8Array([1])), 1)).toBeNull();
    expect(packetCode(() => assembler.push(encodedPacket(1, 2, 0, new Uint8Array([2])), 2))).toBe("fragment-order");
    expect(assembler.isClosed).toBe(true);
  });

  test("keeps incoming and outgoing reservations separate and releases a stalled partial", () => {
    const quota = new RecordingQuota(ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES + 1);
    const incoming = new AttachmentTransferPacketAssembler("incoming", quota);
    const outgoing = new AttachmentTransferPacketQueue("outgoing", quota);
    const source = new Uint8Array(ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES + 1);
    expect(incoming.push(
      encodedPacket(1, source.byteLength, 0, source.slice(0, ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES)),
      100,
    )).toBeNull();
    expect(outgoing.enqueue(source)).toBe(true);
    expect(quota.reserved).toEqual({ incoming: source.byteLength, outgoing: source.byteLength });
    expect(incoming.expire(100 + ATTACHMENT_TRANSFER_PACKET_STALL_MS - 1)).toBe(false);
    expect(incoming.expire(100 + ATTACHMENT_TRANSFER_PACKET_STALL_MS)).toBe(true);
    expect(quota.reserved).toEqual({ incoming: 0, outgoing: source.byteLength });
    outgoing.clear();
    expect(quota.reserved).toEqual({ incoming: 0, outgoing: 0 });
  });

  test("rejects absent, non-finite, and negative clock values", () => {
    const packet = encodedPacket(1, 1, 0, new Uint8Array([1]));
    const absentClock = new AttachmentTransferPacketAssembler(
      "incoming",
      new RecordingQuota(),
      () => undefined as unknown as number,
    );
    expect(packetCode(() => absentClock.push(packet))).toBe("clock");
    expect(absentClock.isClosed).toBe(true);

    for (const nowMs of [Number.NaN, Number.POSITIVE_INFINITY, -1]) {
      const assembler = new AttachmentTransferPacketAssembler("incoming", new RecordingQuota());
      expect(packetCode(() => assembler.push(packet, nowMs))).toBe("clock");
      expect(assembler.isClosed).toBe(true);
    }
  });
});

describe("attachment transfer packet queue", () => {
  test("caches each fragment until accepted-once commit and never duplicates a buffered send", () => {
    const quota = new RecordingQuota();
    const queue = new AttachmentTransferPacketQueue("outgoing", quota);
    const source = Uint8Array.from({ length: ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES + 1 }, () => 1);
    expect(queue.enqueue(source)).toBe(true);

    const first = queue.nextFragment()!;
    expect(queue.nextFragment()).toBe(first);
    expect(first.bytes.byteLength).toBe(ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES + ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES);
    first.commit();
    first.commit();

    const final = queue.nextFragment()!;
    expect(parseAttachmentTransferPacket(final.bytes)).toMatchObject({
      messageId: 1,
      totalBytes: source.byteLength,
      offsetBytes: ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES,
    });
    expect(final.final).toBe(true);
    final.commit();
    expect(queue.nextFragment()).toBeNull();
    expect(quota.reserved.outgoing).toBe(0);
    expect(quota.releaseCalls).toEqual([{ direction: "outgoing", bytes: source.byteLength }]);
  });

  test("refuses quota admission before ownership transfer and resets after close", () => {
    const quota = new RecordingQuota(1);
    const queue = new AttachmentTransferPacketQueue("outgoing", quota);
    expect(queue.enqueue(new Uint8Array([1, 2]))).toBe(false);
    expect(queue.messageCount).toBe(0);

    queue.clear();
    expect(packetCode(() => queue.enqueue(new Uint8Array([3])))).toBe("closed");
    queue.reset();
    expect(queue.enqueue(new Uint8Array([4]))).toBe(true);
    const fragment = queue.nextFragment()!;
    expect(parseAttachmentTransferPacket(fragment.bytes).messageId).toBe(1);
    fragment.commit();
    expect(quota.reserved.outgoing).toBe(0);
  });
});
