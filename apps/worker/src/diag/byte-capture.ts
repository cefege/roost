// Per-session ring buffer of the last 256KB of PTY-output bytes.
// `push(sid, chunk, end_seq)` runs on every chunk retained in a session's
// scrollback (session-scrollback.ts::retainRaw). `snapshotByteCapture(sid)`
// hands the incident bundle writer an OWNED copy of that tail plus its
// absolute start/end offsets; `drop(sid)` frees the ring on session close.
//
// `push` is deliberately always-on and unconditional on ROOST_DIAG: an anomaly
// fires precisely when diag was off, and a capture of an empty ring explains
// nothing. It costs O(chunk) because the ring is a fixed-capacity SbRing.
//
// This module owns NO file: directory security, naming and retention belong to
// diag/capture-storage.ts, and the only thing that reads the tail is
// diag/terminal-capture-worker-section.ts.

import type { TerminalWorkerByteCaptureTail } from "@roost/shared/terminal-capture";
import {
	createSbRing,
	appendToRing,
	readRing,
	type SbRing,
} from "../session-scrollback-ring.ts";

const RING_CAP_BYTES = 256 * 1024;

interface RingEntry {
	ring: SbRing;
	end_seq: number;
}

const _rings = new Map<string, RingEntry>();

/** Append `chunk` to the per-sid ring. Oldest bytes are overwritten in place
 *  once RING_CAP_BYTES is retained. O(chunk), one fixed allocation per sid. */
export function push(sid: string, chunk: Uint8Array, endSeq: number): void {
	let entry = _rings.get(sid);
	if (!entry) {
		entry = { ring: createSbRing(undefined, RING_CAP_BYTES), end_seq: 0 };
		_rings.set(sid, entry);
	}
	appendToRing(entry.ring, chunk);
	entry.end_seq = endSeq;
}

/** Drop the ring for `sid`. Called on session close. */
export function drop(sid: string): void {
	_rings.delete(sid);
}

/** The retained raw tail with its absolute bounds, or null when this session
 *  has produced no output. The start offset is derived from the end offset the
 *  ring last stamped minus what it still holds, so an evicted prefix reports an
 *  honest window rather than claiming to start at zero.
 *
 *  The returned bytes are OWNED: `readRing` hands back a view onto the ring for
 *  an unwrapped ring, and a bundle written after a later append would otherwise
 *  carry whatever overwrote it. */
export function snapshotByteCapture(sid: string): TerminalWorkerByteCaptureTail | null {
	const entry = _rings.get(sid);
	if (!entry) return null;
	const retained = readRing(entry.ring);
	if (retained.byteLength === 0) return null;
	const owned = new Uint8Array(retained);
	const endOffset = BigInt(Math.max(entry.end_seq, owned.byteLength));
	return {
		end_offset: endOffset.toString(),
		start_offset: (endOffset - BigInt(owned.byteLength)).toString(),
		byte_length: owned.byteLength,
		base64: Buffer.from(owned).toString("base64"),
	};
}

/** Test-only: clear every ring. */
export function _resetForTest(): void {
	_rings.clear();
}

/** Test-only: inspect ring state. */
export function _getRingForTest(sid: string): RingEntry | undefined {
	return _rings.get(sid);
}
