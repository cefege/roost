// The per-session retained PTY byte window, as an actual fixed-capacity ring.
// Owned by SessionRecord.scrollback; written by appendScrollback on every PTY
// chunk and read by the wterm-core rebuild path (session-viewport.ts).
//
// The grow-and-slice predecessor allocated `retained + chunk` bytes and memcpy'd
// twice per chunk, so appending one byte of keystroke echo to a saturated
// session copied the whole 1 MiB window. Here an append costs O(chunk) and the
// footprint is a single fixed allocation, which is what the memory-tight-host
// failure mode (CLAUDE.md L11 cgroup-OOM row) actually needs.

// 8 MB sliding scrollback window kept on the worker per session.
// Matches the keeper's ring (sb30) so getScrollback can serve a fresh
// SPA the same depth of history the keeper started us with. ~24 KB
// of memory overhead is rounding error for the saved roundtrip.
// 2026-06-22: 8 MB → 1 MB, matched to KEEPER_RING_CAP_BYTES (multiplexed-main.ts).
// Smaller per-channel footprint on a permanently RAM-full box; ~10k lines is
// ample. See memory project_keeper_death_auto_respawn.
// Lives here, not in session-constants.ts, because the keeper subprocess
// imports this module and must not pull in WASM + node:crypto with it.
export const SCROLLBACK_CAP_BYTES = 1 * 1024 * 1024;

const EMPTY = new Uint8Array(0);

/** `buf` is empty until the first append, then exactly `cap` bytes for the
 *  ring's life. `write` is the next write offset; `filled` is how many
 *  bytes are retained. While `filled < buf.length` nothing has wrapped, so the
 *  retained bytes are contiguous at [0, filled) and `write === filled`. */
export interface SbRing {
	buf: Uint8Array;
	write: number;
	filled: number;
	cap: number;
}

/** Seeded from keeper-retained history on resume; unseeded on a fresh spawn.
 *  Allocation is deferred to the first append so a session that never produces
 *  output costs nothing. */
export function createSbRing(seed?: Uint8Array, capBytes: number = SCROLLBACK_CAP_BYTES): SbRing {
	const ring: SbRing = { buf: EMPTY, write: 0, filled: 0, cap: capBytes };
	if (seed && seed.length > 0) appendToRing(ring, seed);
	return ring;
}

/** O(chunk). At capacity the oldest bytes are overwritten in place — callers
 *  advance head_seq by chunk.length regardless, so the monotonic byte offset
 *  the splice path depends on is unaffected by eviction. */
export function appendToRing(ring: SbRing, chunk: Uint8Array): void {
	if (chunk.length === 0) return;
	if (ring.buf.length === 0) ring.buf = new Uint8Array(ring.cap);
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

/** O(1) byte bounds for the diagnostic surface. The ring is the ONLY thing that
 *  bounds a resize replay, so a human comparing "what the browser holds" against
 *  "what the core holds" needs to see how close this ring is to the cap that
 *  will truncate the next rebuild. `cap_bytes` is the CONFIGURED capacity, not
 *  `buf.length`: allocation is deferred to the first append, so an untouched
 *  ring must still report the bound it will enforce. `evicting` is the fact that
 *  matters — once true, every further append drops history the next rebuild
 *  could otherwise have replayed. */
export function ringBounds(ring: SbRing): {
	retained_bytes: number;
	cap_bytes: number;
	evicting: boolean;
} {
	return { retained_bytes: ring.filled, cap_bytes: ring.cap, evicting: ring.filled >= ring.cap };
}
