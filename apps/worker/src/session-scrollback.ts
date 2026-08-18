// Scrollback ring append/replay + the ordered terminal query-reply lane. Split
// out of session-manager.ts (400-line cap); the ring functions are called with
// a SessionManager `this` (see the delegating wrappers in session-manager.ts).

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import { answerQueries, QUERY_CARRY_MAX } from "./terminal-query-reply.ts";
import { diag, isDiagEnabled } from "@roost/shared/diag";
import { supportedHostPlatform } from "@roost/shared/platform";
import * as byteCapture from "./diag/byte-capture.ts";
import { _scanAgentOsc, _scanAltModeTransitions, _scanOsc7 } from "./terminal-stream-scan.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { _sha8, MODE_CARRY_MAX } from "./session-constants.ts";
import { appendToRing } from "./session-scrollback-ring.ts";
import { canonicalExistingWorkerPath } from "./util/path.ts";

const HOST_PLATFORM = supportedHostPlatform();
/** Probe replies are short and rare; the encoder is not, so build it once. */
const REPLY_ENCODER = new TextEncoder();

/** Append a chunk to the per-session scrollback ring, evicting oldest
 *  bytes if the cap is exceeded. Called from attachOutputClient's
 *  onOutput so every PTY byte the worker forwards live is ALSO
 *  retained for serving via getScrollback to fresh SPA mounts.
 *  phase-ssb1: also advances head_seq by chunk.length so the ring
 *  exposes a monotonic byte offset for resumable scrollback.
 *  Returns the head_seq AFTER appending (i.e. the end_seq for this
 *  chunk) so callers can stamp the upstream frame without a second
 *  Map lookup; -1 if the channel is unknown (session killed). */
export function appendScrollback(this: SessionManager, channelId: number, chunk: Buffer): number {
	const rec = this.sessions.get(channelId);
	if (!rec) return -1;
	const core = rec.wtermCore;
	if (!core) return -1;
	const bytes = retainRaw(rec, channelId, chunk);
	// Mirror to the core AND answer the capability probes this chunk carried in
	// ONE ordered pass: the core is fed in segments cut at each probe Roost
	// synthesizes, so a native CPR and a synthesized DA reach the pty in the
	// order the application asked. The reply goes BACK into the pty (stdin).
	// Live chunk ONLY — replay/rebuild sites discard it.
	answerTerminalQueries(rec, channelId, bytes);
	scanStreamState.call(this, rec, chunk, bytes);
	return rec.head_seq;
}

/** Capture-lane ingest. A geometry-uncertain transaction freezes the canonical
 *  core, so a captured chunk does everything the live path does EXCEPT touch the
 *  core or answer its probes: it advances head_seq, enters the fixed raw ring,
 *  and runs the metadata/carry scans. The frozen core would otherwise parse
 *  bytes at a width the worker cannot prove; the deferred probes are answered
 *  once, FIFO, by the post-boundary tail replay (session-resize-capture.ts). */
export function appendCapturedScrollback(this: SessionManager, channelId: number, chunk: Buffer): number {
	const rec = this.sessions.get(channelId);
	if (!rec?.wtermCore) return -1;
	const bytes = retainRaw(rec, channelId, chunk);
	// The frozen core never parses these bytes, but the stream still advanced:
	// carry the tokenizer across them so a partial probe cannot be glued onto
	// the first post-rebuild chunk. Their own probes are answered once, FIFO, by
	// the post-boundary tail replay.
	noteDroppedCarry(rec, channelId, answerQueries(rec, null, bytes).droppedCarry);
	scanStreamState.call(this, rec, chunk, bytes);
	return rec.head_seq;
}

/** Retain one chunk in the ring, advance head_seq, feed the always-on byte
 *  capture. Shared by the live and capture lanes so the retained window is
 *  identical either way — the ring is the single source of truth for the
 *  rebuild, and a captured byte that skipped it would be lost history. */
function retainRaw(rec: SessionRecord, channelId: number, chunk: Buffer): Uint8Array {
	const bytes = new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
	appendToRing(rec.scrollback, chunk);
	rec.head_seq += chunk.length;
	// Diag: per-chunk byte capture into a fixed-capacity 256 KB SbRing, so the
	// push is O(chunk). Deliberately always-on rather than ROOST_DIAG-gated —
	// an anomaly fires when diag was off, and the tail is only ever read by
	// byteCapture.dump, which must not find an empty ring.
	byteCapture.push(String(rec.sessionId), bytes, rec.head_seq);
	// `diag` is a no-op function when the firehose is off, but its ARGUMENTS
	// always evaluate — so _sha8 ran a real sha256 over every PTY chunk on the
	// default production path. The guard is what makes it free.
	if (isDiagEnabled()) {
		diag("bytes.chunk", {
			sid: rec.sessionId,
			channel_id: channelId,
			session_trace_id: rec.session_trace_id,
			dir: "down",
			end_seq: rec.head_seq,
			len: chunk.length,
			sha8: _sha8(chunk),
			alt_mode: rec.alt_mode,
		});
	}
	return bytes;
}

/** WHICH history floor a scrollback read hit — the difference between "gone
 *  forever" and "lost to the replay bound", which the floor's numeric value
 *  alone cannot tell apart.
 *
 *  `requestedStart` is the oldest absolute row the caller asked for and `floor`
 *  the LIVE monotonic floor (scrollbackOrigin) the read resolved through:
 *
 *   - "none": the caller never reached below the floor, so no history is missing.
 *   - "evicted": the core's own line ring rolled past those rows as newer output
 *     arrived. Gone forever — no ring, no replay, nothing recovers them.
 *   - "resize_replay": the floor is still exactly where a rebuild's replay left
 *     it, and that replay was bounded by the fixed byte ring rather than by the
 *     core's line ring. A session that was never resized would STILL hold those
 *     rows, so the loss is resize-induced, not genuine.
 *
 *  The floor only ever rises, so one comparison against the pin's watermark
 *  decides: still equal ⇒ the replay owns this floor; higher ⇒ ordinary eviction
 *  has since carried the floor past whatever the replay left behind, and what a
 *  caller hits now is an eviction floor. O(1), and no per-read state. */
export function historyFloorReason(
	rec: SessionRecord,
	requestedStart: number,
	floor: number,
): ScrollbackHistoryFloor {
	if (floor <= 0 || requestedStart >= floor) return "none";
	// replay_floor is 0 until a replay actually loses rows, so it doubles as the
	// "no replay has ever bounded this session's history" sentinel.
	const replayFloor = rec.sb_origin_pin?.replay_floor ?? 0;
	if (replayFloor > 0 && floor === replayFloor) return "resize_replay";
	return "evicted";
}

/** Alt-screen mode, agent OSC, and OSC 7 cwd tracking. Stream state, not grid
 *  state, so it stays truthful while the core is frozen. */
function scanStreamState(
	this: SessionManager,
	rec: SessionRecord,
	chunk: Buffer,
	bytes: Uint8Array,
): void {
	// phase-ssb-altmode: scan for DEC private mode 1049/47/1047
	// transitions. mode_carry bridges the last MODE_CARRY_MAX bytes across
	// chunk boundaries.
	const combined = new Uint8Array(rec.mode_carry.length + chunk.length);
	combined.set(rec.mode_carry, 0);
	combined.set(bytes, rec.mode_carry.length);
	rec.alt_mode = _scanAltModeTransitions(combined, rec.alt_mode);
	rec.mode_carry =
		combined.length > MODE_CARRY_MAX
			? combined.subarray(combined.length - MODE_CARRY_MAX)
			: combined;
	const agentOsc = _scanAgentOsc(
		rec.agentOscCarry + rec.agentOscDecoder.decode(bytes, { stream: true }),
	);
	rec.agentOscCarry = agentOsc.carry;
	if (agentOsc.title !== null) rec.rawOscTitle = agentOsc.title;
	if (agentOsc.progress !== null) rec.rawOscProgress = agentOsc.progress;
	// OSC 7 cwd tracking — combine the prior carry with this chunk so
	// a sequence straddling a chunk boundary still parses. On a new
	// path, emit a `cwd` SessionEvent so coord persists session.cwd
	// and every browser sees the update via the events stream.
	const osc7Combined = new Uint8Array(rec.osc7_carry.length + chunk.length);
	osc7Combined.set(rec.osc7_carry, 0);
	osc7Combined.set(bytes, rec.osc7_carry.length);
	const { newCwd, carry } = _scanOsc7(osc7Combined, HOST_PLATFORM);
	rec.osc7_carry = carry;
	let canonicalCwd = newCwd;
	if (canonicalCwd && HOST_PLATFORM === "win32") {
		try {
			canonicalCwd = canonicalExistingWorkerPath(canonicalCwd, HOST_PLATFORM);
		} catch {
			// The directory can disappear between prompt emission and scan.
			// Keep the already-normalized OSC path rather than dropping the cwd.
		}
	}
	if (canonicalCwd && canonicalCwd !== rec.cwd) {
		rec.cwd = canonicalCwd;
		this.emitEvent({
			kind: "cwd",
			session_id: rec.sessionId,
			cwd: canonicalCwd,
			ts: Date.now(),
		});
		// New cwd may be a different repo/branch — re-resolve + re-watch.
		rec.gitWatchDispose?.();
		rec.gitWatchDispose = null;
		this._startGitBranch(rec);
		this._startPorts(rec);
	}
}

// cell-phase-4: getScrollbackForViewer / getScrollbackSince retired — cell frames
// are the sole output path. Scrollback backfill now goes through
// getScrollbackCells (cell rows) via handleGetScrollbackCells.
// serializeWTerm stays as a test utility in wterm-serialize.ts.

/** Answer the capability probes a LIVE chunk carried, writing the replies BACK
 *  into the pty (stdin) as one batch. `answerQueries` owns the core write: it
 *  feeds the chunk in segments cut at each synthesized probe and drains the
 *  core's own queued replies between them, so natives and synthesized replies
 *  reach the application in the order it asked for them. No-op when the chunk
 *  held no probe and the core had nothing queued. */
function answerTerminalQueries(rec: SessionRecord, channelId: number, chunk: Uint8Array): void {
	const reply = answerQueries(rec, rec.wtermCore, chunk);
	noteDroppedCarry(rec, channelId, reply.droppedCarry);
	if (reply.bytes.length === 0) return;
	getMultiplexedPool().input(channelId, REPLY_ENCODER.encode(reply.bytes));
	diag("terminal.query_reply", {
		channel_id: channelId,
		native_len: reply.native.length,
		synth_len: reply.synth.length,
	});
}

/** A CSI that outgrew the bounded carry without ever reaching its final byte:
 *  the partial is abandoned rather than allowed to pin memory, so say so. */
function noteDroppedCarry(rec: SessionRecord, channelId: number, dropped: number): void {
	if (dropped === 0) return;
	diag("terminal.query_carry_dropped", {
		sid: rec.sessionId,
		channel_id: channelId,
		bytes: dropped,
		cap: QUERY_CARRY_MAX,
	});
}
