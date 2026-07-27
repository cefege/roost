// Upstream PTY-byte and cell-grid emission. Called with a SessionManager `this`
// (see wrappers in session-manager.ts).

import type { SessionManager } from "./session-manager.ts";
import type { ChannelId } from "@roost/shared";
import { log, diag, signal } from "@roost/shared";
import { DIR_FROM_PTY } from "@roost/shared/wire";
import { nextCellFrame, SB_SNAPSHOT_TAIL_ROWS } from "@roost/shared/cell";
import { cellFrameToProto } from "@roost/shared/cell/cell-proto";
import type { MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import {
	RECENTLY_CLOSED_TTL_MS,
	KEEPER_DEGRADED_WINDOW_MS,
	KEEPER_DEGRADED_THRESHOLD,
	CELL_EMIT_COALESCE_MS,
} from "./session-constants.ts";


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
	// Worker-side terminal handling ends here. Structured agent state is supplied
	// by the OMP bridge, not inferred from an arbitrary terminal grid.
}


/** Is any live viewer claiming this channel? Withdrawn viewers are removed
 *  (deferred withdraw) and crashed ones reaped at VIEWPORT_CLAIM_TTL_MS, so a
 *  non-empty claim set = a real watcher. Drives B's "don't emit to nobody". */
export function _hasActiveViewer(this: SessionManager, channelId: number): boolean {
	return (this.viewportClaims.get(channelId)?.size ?? 0) > 0;
}

/** Rate governor: leading-edge cell emit plus trailing coalesce.
 * The FIRST chunk in a burst queues a microtask after its UDS read settles into
 * wtermCore. A trailing CELL_EMIT_COALESCE_MS timer absorbs subsequent chunks. */
export function _scheduleCellEmit(this: SessionManager, channelId: number): void {
	// Trailing burst — a leading emit already fired this tick, or a trailing
	// timer is pending. Absorb: wtermCore already holds these bytes.
	if (this.cellEmitTimers.has(channelId)) { this.cellDirty.add(channelId); return; }

	// Leading edge: queue a microtask so all PtyOut chunks delivered in this
	// the common single-keystroke echo while preserving coalescing for bursts.
	this.cellEmitTimers.set(channelId, LEADING_SENTINEL);
	queueMicrotask(() => {
		this.cellEmitTimers.delete(channelId);
		if (!this.sessions.has(channelId)) return;
		this.emitCellFrame(channelId, false);
		// Trailing coalesce: absorb chunks for CELL_EMIT_COALESCE_MS, flush the
		// latest grid, then RE-ARM while the channel is still producing. Without
		// the re-arm the window closes after one flush and the next chunk starts
		// a fresh leading edge microseconds later — two frames per 16ms, double
		// the intended rate, and every frame is one scroll re-derive on every
		// viewer. A channel that goes quiet lets the armed timer fire once,
		// find nothing dirty, and stop.
		const arm = (): void => {
			const timer = setTimeout(() => {
				this.cellEmitTimers.delete(channelId);
				if (!this.sessions.has(channelId)) return;
				if (!this.cellDirty.has(channelId)) return;
				this.emitCellFrame(channelId, false);
				arm();
			}, CELL_EMIT_COALESCE_MS);
			this.cellEmitTimers.set(channelId, timer);
		};
		arm();
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
export function emitCellFrame(this: SessionManager, channelId: number, force: boolean, tailRows?: number): void {
	const send = this.sendCellGridUpstream;
	if (!send) return;
	const rec = this.sessions.get(channelId);
	if (!rec) return;
	// The live paths create a terminal core before registration. Keep this
	// narrow for teardown races and sparse test fixtures.
	const core = rec.wtermCore;
	if (!core) return;
	const pending = this.cellEmitTimers.get(channelId);
	if (pending !== undefined) {
		if (pending !== LEADING_SENTINEL) clearTimeout(pending);
		this.cellEmitTimers.delete(channelId);
	}
	const { frame, state } = nextCellFrame(core, rec.cell_emit, force, tailRows ?? SB_SNAPSHOT_TAIL_ROWS);
	rec.cell_emit = state;
	core.clearDirty();
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
	pb.ptyOutMs = BigInt(Date.now());
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
