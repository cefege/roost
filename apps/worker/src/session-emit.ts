// Upstream byte/cell emit + herdr agent-status detection. Split out of
// session-manager.ts (400-line cap); bodies byte-for-byte unchanged, called
// with a SessionManager `this` (see wrappers in session-manager.ts).

import type { SessionManager } from "./session-manager.ts";
import type { ChannelId } from "@roost/shared";
import { log, diag, signal } from "@roost/shared";
import { DIR_FROM_PTY } from "@roost/shared/wire";
import { nextCellFrame, SB_SNAPSHOT_TAIL_ROWS } from "@roost/shared/cell";
import { cellFrameToProto } from "@roost/shared/cell/cell-proto";
import { detectAgentScreen, screenStatus } from "./detect/screen-detect.ts";
import { resolveAgentStatus } from "./detect/arbiter.ts";
import type { MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import {
	extractOscTitleStateful,
	RECENTLY_CLOSED_TTL_MS,
	KEEPER_DEGRADED_WINDOW_MS,
	KEEPER_DEGRADED_THRESHOLD,
	DETECT_DEBOUNCE_MS,
	AGENT_WORKING_GRACE_MS,
	CELL_EMIT_COALESCE_MS,
} from "./session-constants.ts";

// Empty carry seed for extractOscTitleStateful (per-channel OSC title bridging).
const EMPTY_OSC_CARRY = new Uint8Array(0);

// Leading-edge sentinel: marks a microtask-queued cell emit in cellEmitTimers.
// clearTimeout(LEADING_SENTINEL) is a no-op (coerces to NaN), so existing
// clearTimeout calls on a pending sentinel are safe.
const LEADING_SENTINEL = -1 as unknown as ReturnType<typeof setTimeout>;

// phase-ssb7: emitScrollbackMark + DIR_SCROLLBACK_MARK deleted.
// Splice ordering is now per-byte end_seq on each FROM_PTY frame
// (attachOutputClient.onOutput below). See CLAUDE.md L11 "scrollback
// seam torn" row.

/** Compose the upstream binary frame for a PTY chunk.
 *  Wire format: [u16 BE channelId][u8 DIR_FROM_PTY][u64 BE endSeq][bytes].
 *  Shared by attachOutputClient's legacy keeper callback AND
 *  muxCallbacks' pool keeper callback so any wire-format change
 *  lands in one place — guards the L11 scrollback-seam path. */
export function emitUpstreamChunk(this: SessionManager, channelId: number, chunk: Buffer): void {
	const rec = this.sessions.get(channelId);
	diag("cell.recv", { sid: String(rec?.sessionId ?? ""), channel_id: channelId, len: chunk.length });
	if (!this.sendBinaryUpstream) {
		log.warn("session-manager", "emit_no_upstream", {
			channelId,
			len: chunk.length,
		});
		return;
	}
	const endSeq = this.appendScrollback(channelId, chunk);
	if (endSeq < 0) {
		// Post-close tail: bytes for a channel we deleted < RECENTLY_CLOSED_TTL_MS
		// ago. Benign (keeper is a separate process; in-flight frames arrive after
		// teardown). Drop silently — do NOT count toward _noSessionBurst, or the
		// tail re-trips keeper.degraded after every reconcile → restart loop.
		const closedAt = this.recentlyClosed.get(channelId);
		if (
			closedAt !== undefined &&
			Date.now() - closedAt < RECENTLY_CLOSED_TTL_MS
		) {
			diag("session.tail_drop", { channel_id: channelId, len: chunk.length });
			return;
		}
		log.warn("session-manager", "emit_no_session", {
			channelId,
			len: chunk.length,
		});
		// Burst of emit_no_session PAST the tail window = the keeper is emitting on
		// channels this worker no longer maps — the degraded survivor-keeper class
		// (births dead PTYs after a long uptime; CLAUDE.md keeper-death memory).
		// Promote a sustained burst to a Tier-1 signal so `roost doctor` flags it.
		this._noSessionBurst.push(Date.now());
		const cutoff = Date.now() - KEEPER_DEGRADED_WINDOW_MS;
		while (this._noSessionBurst.length && this._noSessionBurst[0]! < cutoff)
			this._noSessionBurst.shift();
		if (this._noSessionBurst.length >= KEEPER_DEGRADED_THRESHOLD) {
			signal("keeper.degraded", {
				no_session_count: this._noSessionBurst.length,
				window_ms: KEEPER_DEGRADED_WINDOW_MS,
				cooldownKey: "keeper",
			});
			// Self-heal: ask the worker to restart the keeper (grace-gated in main.ts
			// so the transient post-reconcile burst can't loop).
			this.onKeeperDegraded?.();
		}
		return; // session killed mid-output
	}
	const frame = Buffer.allocUnsafe(11 + chunk.length);
	frame.writeUInt16BE(channelId, 0);
	frame[2] = DIR_FROM_PTY;
	frame.writeBigUInt64BE(BigInt(endSeq), 3);
	chunk.copy(frame, 11);
	this.sendBinaryUpstream(
		new Uint8Array(frame.buffer, frame.byteOffset, frame.length),
	);
	log.debug("session-manager", "emit_upstream", {
		channelId,
		len: chunk.length,
		endSeq,
	});
	// R11 — emit the cell-grid delta for this chunk (appendScrollback above
	// already wrote the bytes into rec.wtermCore, so the grid + dirty rows
	// are current). Parallel to bytes; gated off by default. Phase-3: coalesce
	// — a burst of chunks emits ONE delta to the latest grid, not one per chunk.
	// B (draw only to attached clients): skip the delta when NO viewer is
	// watching — the grid still updates in wtermCore, and a viewer attaching
	// re-claims → emitCellSnapshot repaints the whole grid. Saves CPU+wire for
	// background sessions (the many-parallel-agents case).
	if (this._hasActiveViewer(channelId)) this._scheduleCellEmit(channelId);
	// herdr agent-status detection. Runs on EVERY session, not just kind:"claude"
	// — users run claude INSIDE a shell session (the worker only tags a pane
	// "claude" when spawned via spawnClaude), so gating on kind blinds the common
	// case. The claude manifest is the real filter: a plain shell screen yields
	// `unknown` → no status emitted. NOT viewer-gated — an idle background agent
	// must still light its sidebar chip; the byte clock feeds the working→idle hold.
	if (this.sessions.has(channelId)) {
		const { title, carry } = extractOscTitleStateful(this.oscTitleCarry.get(channelId) ?? EMPTY_OSC_CARRY, chunk);
		this.oscTitleCarry.set(channelId, carry);
		if (title !== null) {
			this.lastOscTitle.set(channelId, title);
			this._ensureChatWatch(channelId);
		}
		this.lastByteAt.set(channelId, Date.now());
		this._scheduleDetect(channelId);
	}
}

/** Arm a debounced screen-scrape for a claude channel; a byte burst coalesces
 *  into one read of the settled grid (DETECT_DEBOUNCE_MS). */
export function _scheduleDetect(this: SessionManager, channelId: number): void {
	if (this.detectTimers.has(channelId)) return;
	const timer = setTimeout(() => {
		this.detectTimers.delete(channelId);
		this._runDetect(channelId);
	}, DETECT_DEBOUNCE_MS);
	this.detectTimers.set(channelId, timer);
}

/** herdr idle re-scan: re-run detection on every live session so idle agents
 *  (no byte activity to trigger the byte-path scrape) still surface a status.
 *  _runDetect dedups, so steady sessions re-emit nothing. */
export function _sweepDetect(this: SessionManager): void {
	for (const channelId of this.sessions.keys()) this._runDetect(channelId);
}

/** Scrape rec.wtermCore via the herdr engine, arbitrate screen + byte-activity
 *  into a stable status, emit upstream on CHANGE. reevalForIdle re-checks once
 *  the stream goes quiet so a held working→idle edge eventually commits. */
export function _runDetect(this: SessionManager, channelId: number): void {
	const rec = this.sessions.get(channelId);
	if (!rec) return;
	// A record mid-teardown (or a stale one a leaked sweep timer still holds)
	// can have no wtermCore — nothing to scrape, skip rather than crash.
	if (!rec.wtermCore) return;
	const det = detectAgentScreen(
		rec.wtermCore,
		this.lastOscTitle.get(channelId),
	);
	const recentBytes =
		Date.now() - (this.lastByteAt.get(channelId) ?? 0) <
		AGENT_WORKING_GRACE_MS;
	const prev = this.committedStatus.get(channelId);
	const { next, reevalForIdle } = resolveAgentStatus({
		prev,
		screenStatus: screenStatus(det),
		screenBlocker: det.visibleBlocker,
		recentBytes,
	});
	if (next !== undefined && next !== prev) {
		this.committedStatus.set(channelId, next);
		this.sendClaudeStatusUpstream?.(channelId, next);
		log.debug("session-manager", "claude_status", {
			channelId,
			status: next,
		});
	}
	if (reevalForIdle && !this.reevalTimers.has(channelId)) {
		const t = setTimeout(() => {
			this.reevalTimers.delete(channelId);
			this._runDetect(channelId);
		}, AGENT_WORKING_GRACE_MS);
		this.reevalTimers.set(channelId, t);
	}
}

/** Re-emit the last committed claude_status for every channel. Called on coord
 *  (re)connect (onHelloAck): the coord's claudeStatusBus + snapshot cache are
 *  in-memory and empty after a coord restart, and detection only emits on
 *  CHANGE — so without this an idle claude stays invisible (shows as a plain
 *  terminal) until its next transition. The coord primes its channel→session
 *  map from the DB on the same hello, so these frames map correctly. */
export function resendClaudeStatuses(this: SessionManager): void {
	for (const [channelId, status] of this.committedStatus) {
		this.sendClaudeStatusUpstream?.(channelId, status);
	}
}

/** Is any live viewer claiming this channel? Withdrawn viewers are removed
 *  (deferred withdraw) and crashed ones reaped at VIEWPORT_CLAIM_TTL_MS, so a
 *  non-empty claim set = a real watcher. Drives B's "don't emit to nobody". */
export function _hasActiveViewer(this: SessionManager, channelId: number): boolean {
	return (this.viewportClaims.get(channelId)?.size ?? 0) > 0;
}

/** Phase-3 (rate governor): leading-edge cell emit + trailing coalesce.
 *  The FIRST chunk in a burst queues a microtask (fires at end of the current
 *  event-loop tick, after all PtyOut chunks from the same UDS read have settled
 *  into wtermCore) so the grid read is complete. Eliminates the 16ms delay for
 *  the common single-keystroke echo. A trailing CELL_EMIT_COALESCE_MS timer
 *  then absorbs subsequent chunks (ls, claude repaint) and flushes the latest
 *  grid once — bounds frame rate under floods. A forced full frame
 *  (emitCellFrame force=true) cancels any pending timer/sentinel — it already
 *  covers the latest. */
export function _scheduleCellEmit(this: SessionManager, channelId: number): void {
	// Trailing burst — a leading emit already fired this tick, or a trailing
	// timer is pending. Absorb: wtermCore already holds these bytes.
	if (this.cellEmitTimers.has(channelId)) { this.cellDirty.add(channelId); return; }

	// Leading edge: queue a microtask so all PtyOut chunks delivered in this
	// event-loop tick settle into wtermCore before the grid read. Eliminates
	// the 16ms delay for the first echo chunk while preserving coalescing for
	// multi-chunk bursts (ls, claude repaint).
	this.cellEmitTimers.set(channelId, LEADING_SENTINEL);
	queueMicrotask(() => {
		this.cellEmitTimers.delete(channelId);
		if (!this.sessions.has(channelId)) return;
		this.emitCellFrame(channelId, false);
		// Trailing coalesce: absorb chunks for CELL_EMIT_COALESCE_MS after the
		// leading emit, then flush the latest grid. Bounds frame rate under floods.
		const timer = setTimeout(() => {
			this.cellEmitTimers.delete(channelId);
			if (this.sessions.has(channelId) && this.cellDirty.has(channelId)) this.emitCellFrame(channelId, false);
		}, CELL_EMIT_COALESCE_MS);
		this.cellEmitTimers.set(channelId, timer);
	});
}

/** R11. Emit a cell frame upstream for `channelId`. Full frame on
 *  first emit / reframe (dims change, scrollback shrink, rebuild) or when
 *  forced (fresh viewer attach); delta from core.isDirtyRow otherwise.
 *  Full frames carry only a SB_SNAPSHOT_TAIL_ROWS scrollback tail (sbBase);
 *  viewers pull the rest via get-scrollback-cells — attach/resize cost stops
 *  scaling with history depth. clearDirty() AFTER reading so the next delta
 *  carries only new changes — the worker's wtermCore dirty bits have no
 *  other consumer. */
export function emitCellFrame(this: SessionManager, channelId: number, force: boolean): void {
	const send = this.sendCellGridUpstream;
	if (!send) return;
	const rec = this.sessions.get(channelId);
	if (!rec) return;
	const pending = this.cellEmitTimers.get(channelId);
	if (pending !== undefined) {
		if (pending !== LEADING_SENTINEL) clearTimeout(pending);
		this.cellEmitTimers.delete(channelId);
	}
	const { frame, state } = nextCellFrame(rec.wtermCore, rec.cell_emit, force, SB_SNAPSHOT_TAIL_ROWS);
	rec.cell_emit = state;
	rec.wtermCore.clearDirty();
	this.cellDirty.delete(channelId);
	diag("cell.emit", {
		sid: String(rec.sessionId),
		seq: frame.seq,
		full: frame.full,
		vp_rows: frame.viewportRows.length,
		sb_append: frame.scrollbackAppend.length,
		sb_rows: frame.scrollbackRows.length,
		sb_base: frame.sbBase,
		cursor_row: frame.cursorRow,
		cursor_col: frame.cursorCol,
		cursor_vis: frame.cursorVisible,
		alt: frame.altScreen,
	});
	// session_id left empty: coord's publishCellGrid stamps it from the
	// channel→session map (byte-hub.ts), overwriting anything sent here.
	const pb = cellFrameToProto(frame, "");
	pb.ptyOutMs = BigInt(this.lastByteAt.get(channelId) ?? 0);
	pb.workerEmitMs = BigInt(Date.now());
	send(channelId, pb);
}

/** Register per-channel output handlers on the multiplexed pool. */
export function muxCallbacks(this: SessionManager, channelId: number): MuxChannelCallbacks {
	return {
		onOutput: (chunk: Buffer) => this.emitUpstreamChunk(channelId, chunk),
		onExit: (exitCode) => {
			if (exitCode === null) {
				const sid = this.sessions.get(channelId)?.sessionId;
				diag("session.exit_null", { sid, channel_id: channelId });
				return;
			}
			this.closedByKeeper(channelId as ChannelId, exitCode);
		},
		onError: (err: Error) =>
			log.warn("session-manager", "mux_channel_err", {
				channelId,
				err: err.message,
			}),
	};
}
