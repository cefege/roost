// Scrollback ring append/replay + terminal query-reply. Split out of
// session-manager.ts (400-line cap); bodies byte-for-byte unchanged. Each
// function is called with a SessionManager `this` (see the delegating
// wrappers in session-manager.ts).

import type { SessionManager } from "./session-manager.ts";
import type { TerminalCore } from "@wterm/core";
import { diag, isDiagEnabled, signal } from "@roost/shared";
import { supportedHostPlatform } from "@roost/shared/platform";
import * as byteCapture from "./diag/byte-capture.ts";
import { synthQueryReplies } from "./terminal-query-reply.ts";
import { _scanAgentOsc, _scanAltModeTransitions, _scanOsc7 } from "./terminal-stream-scan.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { _sha8, MODE_CARRY_MAX } from "./session-constants.ts";
import { appendToRing } from "./session-scrollback-ring.ts";
import { canonicalExistingWorkerPath } from "./util/path.ts";

const HOST_PLATFORM = supportedHostPlatform();

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
	// Mirror to headless xterm. The serialize addon snapshots whatever
	// we've fed it — its job here is solely to support clean fresh-mount
	// replay at the headless cols/rows. Live deltas still ride the raw
	// ring above.
	core.writeRaw(bytes);
	// Answer terminal capability probes so the app that asked hears back. Two
	// sources: (1) DSR (ESC[6n cursor pos) the core DOES answer → getResponse();
	// (2) Primary DA (ESC[c) + XTVERSION (ESC[>0q) the core does NOT answer →
	// synthesized. The reply goes BACK into the PTY (stdin). Live chunk ONLY —
	// replay/rebuild sites discard it.
	this._answerTerminalQueries(
		core,
		channelId,
		bytes,
	);
	// phase-ssb-altmode: scan for DEC private mode 1049/47/1047
	// transitions so getScrollbackSince can prepend the right enter
	// sequence when the session is currently in alt-screen. mode_carry
	// bridges the last MODE_CARRY_MAX bytes across chunk boundaries.
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
	return rec.head_seq;
}

// cell-phase-4: getScrollbackForViewer / getScrollbackSince retired — cell frames
// are the sole output path. Scrollback backfill now goes through
// getScrollbackCells (cell rows) via handleGetScrollbackCells.
// serializeWTerm stays as a test utility in wterm-serialize.ts.

/** Answer terminal capability probes in a LIVE chunk, writing replies BACK
 *  into the PTY (stdin). Two sources: DSR (ESC[6n) the wterm core answers via
 *  getResponse(); Primary DA (ESC[c) + XTVERSION (ESC[>0q) the core leaves
 *  silent → synthQueryReplies. getResponse() drains-and-clears; the synth scan
 *  is per-chunk (exactly-once). No-op when the chunk held no probe. */
export function _answerTerminalQueries(
	this: SessionManager,
	core: TerminalCore,
	channelId: number,
	chunk: Uint8Array,
): void {
	const dsr = core.getResponse();
	const synth = synthQueryReplies(chunk);
	const reply = (dsr ?? "") + synth;
	if (reply.length === 0) return;
	getMultiplexedPool().input(channelId, new TextEncoder().encode(reply));
	diag("terminal.query_reply", {
		channel_id: channelId,
		dsr_len: dsr?.length ?? 0,
		synth_len: synth.length,
	});
}
