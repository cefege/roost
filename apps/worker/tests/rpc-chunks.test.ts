// ChunkReassembler is the only path a >1 MiB omp frame takes, which means it is
// the code most likely to be wrong and least likely to be noticed: a session
// runs fine for hours and then one big tool result arrives. So the fixtures here
// are produced by re-implementing omp's OWN encoder (v17.1.7
// src/modes/rpc/rpc-frame.ts encodeChunkedRpcFrames) rather than by guessing a
// chunk shape — a true round-trip against the real wire format.
//
// Encoder facts this pins (verified against the installed omp source):
//   - chunking triggers at serializedFrameBytes(json) > 1 MiB, i.e. byteLength+1
//     > 1048576, so the smallest chunked byteLength is exactly 1 MiB and count is
//     never below 4. The reassembler's lower bound mirrors that deliberately.
//   - `byteLength` is the TOTAL utf8 length of the logical JSON, excluding the
//     trailing newline.
//   - payload slices are 256 KiB of BYTES, base64 with standard padding.

import { test, expect } from "bun:test";
import { ChunkReassembler } from "../src/agent/rpc-chunks.ts";
import type { RpcFrame } from "../src/agent/rpc-frame.ts";

const CHUNK_PAYLOAD_BYTES = 256 * 1024;

/** omp's encodeChunkedRpcFrames, reproduced. */
function encodeChunked(frame: object, chunkId = "rpc-1"): RpcFrame[] {
  const json = JSON.stringify(frame);
  const bytes = Buffer.from(json, "utf8");
  const byteLength = bytes.byteLength;
  const count = Math.ceil(byteLength / CHUNK_PAYLOAD_BYTES);
  const out: RpcFrame[] = [];
  for (let index = 0; index < count; index++) {
    out.push({
      type: "rpc_chunk",
      chunkId,
      index,
      count,
      byteLength,
      data: bytes
        .subarray(index * CHUNK_PAYLOAD_BYTES, (index + 1) * CHUNK_PAYLOAD_BYTES)
        .toString("base64"),
    });
  }
  return out;
}

/** A tool_execution_end big enough to chunk, carrying a multi-byte character
 *  positioned to STRADDLE a 256 KiB boundary. Decoding per chunk would split
 *  that character's bytes and corrupt it; this is the fixture that proves the
 *  reassembler concatenates bytes and decodes UTF-8 exactly once. */
function bigFrameStraddlingBoundary(): { frame: object; marker: string } {
  // "→" is 3 UTF-8 bytes, so placing its first byte 1 byte before the chunk
  // edge splits it across two chunks.
  const marker = "→PLUM→";
  const build = (headLength: number) => ({
    type: "tool_execution_end",
    toolCallId: "toolu_chunked",
    toolName: "bash",
    result: {
      content: [{ type: "text", text: `${"a".repeat(headLength)}${marker}${"b".repeat(1024 * 1024)}` }],
      details: {},
    },
    isError: false,
  });
  // The JSON envelope before `text` shifts everything right by an amount that is
  // tedious to hand-count and would silently rot if the fixture changed. Measure
  // it, then correct the pad so the marker lands exactly on the edge.
  const probeLength = CHUNK_PAYLOAD_BYTES;
  const probe = Buffer.from(JSON.stringify(build(probeLength)), "utf8");
  const probeStart = probe.indexOf(Buffer.from(marker, "utf8"));
  const prefixBytes = probeStart - probeLength;
  return { frame: build(CHUNK_PAYLOAD_BYTES - prefixBytes - 1), marker };
}

test("reassembles a >1 MiB frame whose multi-byte char straddles a chunk boundary", () => {
  const { frame, marker } = bigFrameStraddlingBoundary();
  const chunks = encodeChunked(frame);
  expect(chunks.length).toBeGreaterThanOrEqual(4);

  // The fixture is only meaningful if a marker byte really crosses the edge.
  const json = Buffer.from(JSON.stringify(frame), "utf8");
  const markerStart = json.indexOf(Buffer.from(marker, "utf8"));
  const markerEnd = markerStart + Buffer.byteLength(marker, "utf8");
  expect(markerStart).toBeGreaterThan(0);
  expect(markerStart).toBeLessThan(CHUNK_PAYLOAD_BYTES);
  expect(markerEnd).toBeGreaterThan(CHUNK_PAYLOAD_BYTES);

  const r = new ChunkReassembler();
  const results = chunks.map((c) => r.push(c));
  // Every chunk but the last yields nothing.
  expect(results.slice(0, -1).every((x) => x === undefined)).toBe(true);

  const out = results.at(-1);
  expect(out).toBeDefined();
  // Byte-exact round trip, not merely "contains the marker".
  expect(JSON.stringify(out)).toBe(JSON.stringify(frame));
});

test("passes a normal frame straight through and stays reusable", () => {
  const r = new ChunkReassembler();
  const frame: RpcFrame = { type: "agent_end", messages: [] };
  expect(r.push(frame)).toEqual(frame);

  // A full chunked sequence still works on the same instance afterwards.
  const { frame: big } = bigFrameStraddlingBoundary();
  const chunks = encodeChunked(big, "rpc-2");
  const last = chunks.map((c) => r.push(c)).at(-1);
  expect(last).toBeDefined();
});

test("a non-chunk frame mid-sequence is a desync, not silently dropped", () => {
  const { frame } = bigFrameStraddlingBoundary();
  const chunks = encodeChunked(frame);
  const r = new ChunkReassembler();
  expect(r.push(chunks[0]!)).toBeUndefined();
  // An interleaved ordinary frame means the stream desynced: fail loudly rather
  // than emit a frame stitched from two different logical frames.
  expect(() => r.push({ type: "turn_end" })).toThrow(/interrupted/);
  // The partial sequence is dropped, so the next sequence starts clean.
  const chunks2 = encodeChunked(frame, "rpc-3");
  expect(chunks2.map((c) => r.push(c)).at(-1)).toBeDefined();
});

test("rejects out-of-order, foreign-id, and re-sent chunks", () => {
  const { frame } = bigFrameStraddlingBoundary();
  const chunks = encodeChunked(frame);

  // Skipped index.
  const r1 = new ChunkReassembler();
  r1.push(chunks[0]!);
  expect(() => r1.push(chunks[2]!)).toThrow(/mismatch/);

  // A sequence that does not start at 0 — the tail of a sequence whose head we
  // missed must not be adopted as a fresh one.
  const r2 = new ChunkReassembler();
  expect(() => r2.push(chunks[1]!)).toThrow(/index 0/);

  // Two encoders interleaving: same index, different chunkId.
  const r3 = new ChunkReassembler();
  r3.push(chunks[0]!);
  const foreign = encodeChunked(frame, "rpc-other")[1]!;
  expect(() => r3.push(foreign)).toThrow(/mismatch/);

  // A duplicate of the chunk we just took is also an index mismatch.
  const r4 = new ChunkReassembler();
  r4.push(chunks[0]!);
  expect(() => r4.push(chunks[0]!)).toThrow(/mismatch/);
});

test("rejects metadata the real encoder can never produce", () => {
  const { frame } = bigFrameStraddlingBoundary();
  const [first] = encodeChunked(frame);

  // count < 2 and a byteLength under the 1 MiB chunking threshold are both
  // impossible from omp's encoder; accepting them would mean accepting a frame
  // assembled from something that is not an omp chunk sequence.
  for (const bad of [
    { ...first, count: 1 },
    { ...first, byteLength: 1024 },
    { ...first, index: -1 },
    { ...first, chunkId: "" },
    { ...first, byteLength: 65 * 1024 * 1024 },
  ]) {
    expect(() => new ChunkReassembler().push(bad as RpcFrame)).toThrow(/invalid rpc chunk metadata/);
  }

  // Non-base64 / corrupted payloads are refused before any allocation.
  for (const bad of [
    { ...first, data: "not base64!!" },
    { ...first, data: "" },
    { ...first, data: 42 },
  ]) {
    expect(() => new ChunkReassembler().push(bad as RpcFrame)).toThrow(/invalid rpc chunk data/);
  }
});

test("a truncated final chunk fails instead of yielding a half frame", () => {
  const { frame } = bigFrameStraddlingBoundary();
  const chunks = encodeChunked(frame);
  const r = new ChunkReassembler();
  for (const c of chunks.slice(0, -1)) r.push(c);
  // Last chunk arrives short: declared byteLength is then unmet. Truncated JSON
  // must never reach JSON.parse as a "successful" frame.
  const short = { ...chunks.at(-1)!, data: Buffer.from("xy", "utf8").toString("base64") };
  expect(() => r.push(short as RpcFrame)).toThrow(/length mismatch/);
});

test("reset() abandons a partial sequence", () => {
  const { frame } = bigFrameStraddlingBoundary();
  const chunks = encodeChunked(frame);
  const r = new ChunkReassembler();
  r.push(chunks[0]!);
  r.reset();
  // After reset the mid-sequence chunk is no longer expected, so index 1 is now
  // an illegal start — proving the partial state is really gone.
  expect(() => r.push(chunks[1]!)).toThrow(/index 0/);
});
