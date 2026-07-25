// Browser-command handlers: terminal viewport (get-scrollback-since /
// get-scrollback-cells / resize). Extracted from browser-command-handler.ts
// (CLAUDE.md 400-line cap).

import type { ClientControlFrame } from "@roost/shared/wire";
import { diag } from "@roost/shared";
import { readScrollbackRangeCells } from "@roost/shared/cell";
import type { CoordLink } from "./transport/CoordLink.ts";
import type { SessionManager } from "./session-manager.ts";

// Server-side ceiling on rows per get-scrollback-cells response — bounds the
// synchronous per-cell WASM walk (and the rpc-ok JSON) to ~1/5 of the old
// worst-case full frame. The SPA chunks at 1000 (scrollbackBackfill.ts).
const SCROLLBACK_CELLS_MAX_ROWS = 2000;

/** Lazy-history backfill (cell mode): serve pre-rendered scrollback rows
 *  [max(0, end_row - max_rows), end_row) of the CURRENT grid epoch. The
 *  attach full frame carries only a tail (sbBase, session-emit.ts); the SPA
 *  pulls the rest here per-viewer — OFF the broadcast Sync stream, so an
 *  attach never re-blasts history to other viewers. Awaits any queued
 *  rebuild (OPT2-1 chain) so rows come from the post-resize grid; the SPA
 *  validates cols + overlap-row text before splicing and re-pulls on the
 *  next reframe if the epoch moved under us. */
export async function handleGetScrollbackCells(
	frame: Extract<ClientControlFrame, { kind: "get-scrollback-cells" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	let rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "unknown session" });
		return;
	}
	// A dims-change attach queues a full-ring replay into a fresh core; serving
	// mid-rebuild would hand out rows the imminent reframe invalidates. A failed
	// rebuild must not fail the read (the chain swallows its own logging).
	const pendingRebuild = sessionMgr._wtermRebuildChain.get(rec.channelId);
	if (pendingRebuild) {
		try {
			await pendingRebuild;
		} catch {
			// rebuild chain logs its own failures; serve from the current core
		}
		rec = sessionMgr.getBySessionId(frame.session_id);
		if (!rec) {
			coordLink.send({ kind: "rpc-error", request_id, message: "session closed" });
			return;
		}
	}
	const core = rec.wtermCore;
	// Registered sessions own a terminal core. Keep the narrow for teardown
	// races and sparse test fixtures.
	if (!core) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session has no terminal" });
		return;
	}
	try {
		// Monotonic index space (grid-to-cells.ts): the SPA's row indices are
		// sbDropped-based, so clamp the request into [sbDropped, sbDropped+count].
		// A request reaching below sbDropped names rows the ring genuinely
		// dropped; returning fewer rows makes the SPA's overlap check fail and
		// its backfill controller park, which is the correct outcome.
		const sbDropped = rec.cell_emit.sbDropped;
		const total = sbDropped + core.getScrollbackCount();
		const endRow = Math.min(frame.end_row, total);
		const startRow = Math.max(sbDropped, endRow - Math.min(frame.max_rows, SCROLLBACK_CELLS_MAX_ROWS));
		const rows = readScrollbackRangeCells(core, startRow, endRow, sbDropped);
		diag("scrollback.cells", {
			sid: rec.sessionId,
			channel_id: rec.channelId,
			session_trace_id: rec.session_trace_id,
			start_row: startRow,
			end_row: endRow,
			total,
			sb_dropped: sbDropped,
			rows: rows.length,
		});
		coordLink.send({
			kind: "rpc-ok",
			request_id,
			data: { rows, cols: core.getCols(), total, start_row: startRow, end_row: endRow },
		});
	} catch (err) {
		coordLink.send({
			kind: "rpc-error",
			request_id,
			message: err instanceof Error ? err.message : String(err),
		});
	}
}

// cell-phase-4: handleGetScrollbackSince retired — cell-mode scrollback backfill
// serves cell rows via handleGetScrollbackCells instead.

export function handleResize(
	frame: Extract<ClientControlFrame, { kind: "resize" }>,
	viewer_id: string,
	deps: { sessionMgr: SessionManager },
): void {
	const { sessionMgr } = deps;
	// Viewport claim per (viewer_fp, channel). PTY size = SCD
	// across live claims so two browsers at different window
	// sizes can't ping-pong the running TUI between sizes.
	// cols=0 OR rows=0 = withdraw (SPA fires on tab-hidden,
	// pagehide, blur). See SessionManager.claimViewport.
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (rec)
		sessionMgr.claimViewport(
			rec.channelId,
			viewer_id,
			frame.cols,
			frame.rows,
			frame.client_seq,
			frame.cause,
			frame.held_sb_total,
		);
	return;
}
