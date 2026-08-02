// The per-session retained PTY byte window, as an actual fixed-capacity ring.
// Owned by SessionRecord.scrollback; written by appendScrollback on every PTY
// chunk and read by the wterm-core rebuild path (session-viewport.ts).
//
// The grow-and-slice predecessor allocated `retained + chunk` bytes and memcpy'd
// twice per chunk, so appending one byte of keystroke echo to a saturated
// session copied the whole 1 MiB window. Here an append costs O(chunk) and the
// footprint is a single fixed allocation, which is what the memory-tight-host
// failure mode (CLAUDE.md L11 cgroup-OOM row) actually needs.

import { SCROLLBACK_CAP_BYTES } from "./session-constants.ts";

const EMPTY = new Uint8Array(0);

/** `buf` is empty until the first append, then exactly SCROLLBACK_CAP_BYTES for
 *  the session's life. `write` is the next write offset; `filled` is how many
 *  bytes are retained. While `filled < buf.length` nothing has wrapped, so the
 *  retained bytes are contiguous at [0, filled) and `write === filled`. */
export interface SbRing {
	buf: Uint8Array;
	write: number;
	filled: number;
}

/** Seeded from keeper-retained history on resume; unseeded on a fresh spawn.
 *  Allocation is deferred to the first append so a session that never produces
 *  output costs nothing. */
export function createSbRing(seed?: Uint8Array): SbRing {
	const ring: SbRing = { buf: EMPTY, write: 0, filled: 0 };
	if (seed && seed.length > 0) appendToRing(ring, seed);
	return ring;
}

/** O(chunk). At capacity the oldest bytes are overwritten in place — callers
 *  advance head_seq by chunk.length regardless, so the monotonic byte offset
 *  the splice path depends on is unaffected by eviction. */
export function appendToRing(ring: SbRing, chunk: Uint8Array): void {
	if (chunk.length === 0) return;
	if (ring.buf.length === 0) ring.buf = new Uint8Array(SCROLLBACK_CAP_BYTES);
	const cap = ring.buf.length;
	if (chunk.length >= cap) {
		ring.buf.set(chunk.subarray(chunk.length - cap), 0);
		ring.write = 0;
		ring.filled = cap;
		return;
	}
	const headRoom = cap - ring.write;
	if (chunk.length <= headRoom) {
		ring.buf.set(chunk, ring.write);
	} else {
		ring.buf.set(chunk.subarray(0, headRoom), ring.write);
		ring.buf.set(chunk.subarray(headRoom), 0);
	}
	ring.write = (ring.write + chunk.length) % cap;
	ring.filled = Math.min(cap, ring.filled + chunk.length);
}

/** Retained bytes oldest→newest. Unwrapped rings hand back a VIEW, valid only
 *  until the next append — every caller consumes it synchronously (the rebuild
 *  path documents that same synchronous window as its correctness argument). */
export function readRing(ring: SbRing): Uint8Array {
	if (ring.filled === 0) return EMPTY;
	const cap = ring.buf.length;
	if (ring.filled < cap) return ring.buf.subarray(0, ring.filled);
	if (ring.write === 0) return ring.buf;
	const out = new Uint8Array(cap);
	const tail = cap - ring.write;
	out.set(ring.buf.subarray(ring.write), 0);
	out.set(ring.buf.subarray(0, ring.write), tail);
	return out;
}

export function ringLength(ring: SbRing): number {
	return ring.filled;
}
