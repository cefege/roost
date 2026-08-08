// The scrollback ring must be observationally identical to the grow-and-slice
// buffer it replaced: for any sequence of appends, the retained bytes are the
// last SCROLLBACK_CAP_BYTES of the concatenation, oldest→newest. The wterm-core
// rebuild replays exactly these bytes, so a wrap-boundary bug here reorders or
// duplicates history at a resize — the L11 corruption class.

import { describe, test, expect } from "bun:test";
import { createSbRing, appendToRing, readRing, ringLength, SCROLLBACK_CAP_BYTES } from "../src/session-scrollback-ring.ts";

// The reference implementation: what appendScrollback used to do per chunk.
function naiveAppend(retained: Uint8Array, chunk: Uint8Array, cap: number = SCROLLBACK_CAP_BYTES): Uint8Array {
  const next = new Uint8Array(retained.length + chunk.length);
  next.set(retained, 0);
  next.set(chunk, retained.length);
  return next.length > cap
    ? next.slice(next.length - cap)
    : next;
}

function chunkOf(len: number, seed: number): Uint8Array {
  const out = new Uint8Array(len);
  for (let i = 0; i < len; i++) out[i] = (seed * 31 + i * 7) & 0xff;
  return out;
}

describe("scrollback byte ring", () => {
  test("an unwritten ring holds nothing and allocates nothing", () => {
    const ring = createSbRing();
    expect(ringLength(ring)).toBe(0);
    expect(readRing(ring).length).toBe(0);
    expect(ring.buf.length).toBe(0);
  });

  test("a seed shorter than the cap is retained verbatim", () => {
    const seed = chunkOf(1000, 3);
    const ring = createSbRing(seed);
    expect(ringLength(ring)).toBe(1000);
    expect(readRing(ring)).toEqual(seed);
  });

  test("random chunk sequences totalling 3x the cap match the naive result", () => {
    const ring = createSbRing();
    let reference: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
    let written = 0;
    let seed = 1;
    // Chunk sizes chosen to straddle the wrap boundary at different offsets.
    const sizes = [1, 7, 64, 1023, 4096, 65_537, 3, 131_072];
    while (written < 3 * SCROLLBACK_CAP_BYTES) {
      const size = sizes[seed % sizes.length]!;
      const chunk = chunkOf(size, seed++);
      appendToRing(ring, chunk);
      reference = naiveAppend(reference, chunk);
      written += size;
      expect(ringLength(ring)).toBe(reference.length);
    }
    expect(readRing(ring)).toEqual(reference);
  });

  test("a 4096-byte per-instance cap bounds that ring alone", () => {
    const CAP = 4096;
    const ring = createSbRing(undefined, CAP);
    let reference: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
    let written = 0;
    let seed = 1;
    const sizes = [1, 7, 64, 1023, 4096, 65_537, 3, 131_072];
    while (written < 3 * CAP) {
      const size = sizes[seed % sizes.length]!;
      const chunk = chunkOf(size, seed++);
      appendToRing(ring, chunk);
      reference = naiveAppend(reference, chunk, CAP);
      written += size;
      expect(ringLength(ring)).toBe(reference.length);
    }
    expect(readRing(ring)).toEqual(reference);
    expect(ring.buf.length).toBe(CAP);
    // The module default is untouched: a default ring still holds 1 MiB.
    expect(createSbRing().cap).toBe(SCROLLBACK_CAP_BYTES);
  });

  test("a chunk larger than the cap keeps only its newest cap bytes", () => {
    const ring = createSbRing(chunkOf(500, 9));
    const huge = chunkOf(SCROLLBACK_CAP_BYTES + 12_345, 11);
    appendToRing(ring, huge);
    expect(ringLength(ring)).toBe(SCROLLBACK_CAP_BYTES);
    expect(readRing(ring)).toEqual(huge.subarray(huge.length - SCROLLBACK_CAP_BYTES));
  });

  test("a chunk landing exactly on the boundary does not rotate the read", () => {
    const ring = createSbRing(chunkOf(SCROLLBACK_CAP_BYTES, 5));
    expect(ring.write).toBe(0);
    const tail = chunkOf(SCROLLBACK_CAP_BYTES, 6);
    appendToRing(ring, tail);
    expect(readRing(ring)).toEqual(tail);
  });

  test("an empty chunk is a no-op and never forces the allocation", () => {
    const ring = createSbRing();
    appendToRing(ring, new Uint8Array(0));
    expect(ring.buf.length).toBe(0);
    expect(ringLength(ring)).toBe(0);
  });

  test("the footprint stays one fixed allocation across the cap", () => {
    const ring = createSbRing();
    for (let i = 0; i < 40; i++) appendToRing(ring, chunkOf(100_000, i));
    expect(ring.buf.length).toBe(SCROLLBACK_CAP_BYTES);
    expect(ringLength(ring)).toBe(SCROLLBACK_CAP_BYTES);
  });
});
