// Core-reported unhandled escape sequences, per session.
//
// @wterm/core keeps a fixed 32-entry ring of CSI sequences its dispatcher did
// not recognise (`DebugLogEntry`/`DEBUG_LOG_MAX`, terminal.zig). Roost never
// read it, so "the core silently ignored this sequence" — the failure class
// behind "my TUI renders wrong in Roost but fine in iTerm" — had no telemetry
// anywhere. The corrected reader lives in @roost/shared's core factory
// (0.3.4's own bridge decodes the ring at the wrong offsets); this module is the
// per-session accounting on top of it.
//
// THE RING IS NEVER CLEARED and the ABI exports no way to clear it, so once a
// sequence has been logged it stays in the window for the rest of that core's
// life. Reporting straight off the window would re-fire the same stale entry on
// every frame, so a consumer has to hold its own high-water mark against the
// ring's total — that is what `UnhandledSequenceLog.consumed` is. A normal
// resize no longer rebuilds this core, so the ring and consumer mark share the
// full lifetime of the session.
//
// PARTIAL DETECTOR BY CONSTRUCTION. The core logs unhandled CSI finals
// (DECSCUSR `CSI Ps SP q`, `CSI > … W`, …) but silently drops every OSC other
// than title (0/2) and hyperlink (8), and every DECSET/DECRST mode number it
// does not implement, WITHOUT logging them. An empty list is not proof that the
// core understood everything the application sent.

import { signal } from "@roost/shared/diag";
import { unhandledSequenceRing } from "@roost/shared/wterm-core-factory";
import type { TerminalCore, UnhandledSequence } from "@wterm/core";
import type { SessionRecord, UnhandledSequenceLog } from "./session-record.ts";
import { monoNowMs } from "./util/mono.ts";

/** Distinct sequences retained per core instance. The core's ring holds 32, so a
 *  larger accumulator could only ever fill with sequences the ring had already
 *  overwritten; a smaller one would forget while the core still remembers. */
export const UNHANDLED_SEQ_MAX = 32;

/** Identity for dedupe. `paramCount` is carried separately from `params` because
 *  the core only records the first four parameters — two sequences agreeing on
 *  those four can still differ in how many followed. */
function sequenceKey(seq: UnhandledSequence): string {
	return `${seq.final}|${seq.private}|${seq.paramCount}|${seq.params.join(",")}`;
}

/** Sample the core's ring and report only what this core has never reported.
 *
 *  Steady-state cost: ONE integer WASM call (`getDebugLogCount`) compared against
 *  the mark, allocating nothing — on a clean session and equally on one that has
 *  already logged sequences, which is why this can run on every emitted frame.
 *  The window is only decoded when the ring's total has actually moved. */
export function noteUnhandledSequences(rec: SessionRecord, core: TerminalCore): void {
	const ring = unhandledSequenceRing(core);
	if (ring === null) return; // not a core this worker built; nothing readable
	const total = ring.total();
	if (total === 0) return;
	let log = rec.unhandled;
	if (log === undefined) {
		log = { consumed: 0, entries: [], keys: new Set(), ringDropped: 0, capped: false };
		rec.unhandled = log;
	}
	if (total === log.consumed) return; // nothing new since the last sample
	const mark = log.consumed;
	log.consumed = total;
	// At the cap the accumulator can learn nothing more, so it stops decoding the
	// window entirely — the mark keeps advancing for free.
	if (log.capped) return;
	// Every entry the core logs has a logical index, 0-based, and the retained
	// window holds the newest `retained` of them: window[i] is logical index
	// `oldest + i`. Anything between the mark and `oldest` was overwritten inside
	// the core before Roost got to it — counted, because a session that outruns a
	// 32-entry ring between two frames is itself the finding.
	const retained = total < ring.capacity ? total : ring.capacity;
	const oldest = total - retained;
	if (oldest > mark) log.ringDropped += oldest - mark;
	const window = ring.entries();
	for (let i = mark > oldest ? mark - oldest : 0; i < window.length; i++) {
		const seq = window[i]!;
		const key = sequenceKey(seq);
		if (log.keys.has(key)) continue;
		log.keys.add(key);
		log.entries.push({
			final: seq.final,
			private: seq.private,
			paramCount: seq.paramCount,
			// Decoded fresh per read, not a view into WASM memory.
			params: seq.params,
			firstSeenMonoMs: monoNowMs(),
		});
		signal("terminal.unhandled_sequence", {
			sid: String(rec.sessionId),
			channel_id: rec.channelId,
			final: seq.final,
			private: seq.private,
			param_count: seq.paramCount,
			params: seq.params.join(";"),
			distinct: log.entries.length,
			logged_total: total,
			ring_dropped: log.ringDropped,
			// Per-channel scope: a TUI spraying unknown sequences coalesces into one
			// line per cooldown window while every other session stays independent.
			cooldownKey: String(rec.channelId),
		});
		if (log.entries.length >= UNHANDLED_SEQ_MAX) {
			log.capped = true;
			break;
		}
	}
}

export interface UnhandledSequenceSnapshotEntry {
	final: string;
	private: string;
	param_count: number;
	params: number[];
	first_seen_mono_ms: number;
}

export interface UnhandledSequenceSnapshot {
	/** Distinct sequences, oldest first, never more than UNHANDLED_SEQ_MAX. */
	entries: UnhandledSequenceSnapshotEntry[];
	/** Sequences this core has logged in total, duplicates included. */
	logged_total: number;
	/** Entries the core's 32-entry ring overwrote before Roost read them. */
	ring_dropped: number;
	/** `entries` is full: later DISTINCT sequences were neither recorded nor
	 *  counted, so this is the first UNHANDLED_SEQ_MAX, not necessarily all. */
	capped: boolean;
}

/** Sample, then project — so a PARKED pane, which emits no frames at all, still
 *  answers "what did we ignore?" from one snapshot read. `null` = this core has
 *  logged nothing, which per this module's header is not proof of full support. */
export function unhandledSequenceSnapshot(
	rec: SessionRecord,
	core: TerminalCore,
): UnhandledSequenceSnapshot | null {
	noteUnhandledSequences(rec, core);
	const log = rec.unhandled;
	if (log === undefined) return null;
	return {
		entries: log.entries.map((entry) => ({
			final: entry.final,
			private: entry.private,
			param_count: entry.paramCount,
			params: entry.params,
			first_seen_mono_ms: Math.round(entry.firstSeenMonoMs),
		})),
		logged_total: log.consumed,
		ring_dropped: log.ringDropped,
		capped: log.capped,
	};
}
