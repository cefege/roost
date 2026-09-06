// Browser-command handler for bounded, demand-driven scrollback cell retrieval.
// It serves the worker's authoritative grid through the shared cell codec and
// fences each cooperative page against terminal-control and grid-epoch changes.

import type { ClientControlFrame } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import {
	cellGridEpoch, readScrollbackRangeCells, scrollbackOrigin, type CellRow,
} from "@roost/shared/cell";
import type { CoordLink } from "./transport/coord-link.ts";
import type { SessionManager } from "./session-manager.ts";
import { terminalControlSettled } from "./session-control-lanes.ts";
import { historyFloorReason } from "./session-scrollback.ts";

// Server-side ceiling on rows per get-scrollback-cells response — bounds the
// per-cell WASM walk (and the rpc-ok JSON) for one RPC. The SPA chunks at 1000
// and issues BACKFILL_CONCURRENCY of those per wave (scrollbackBackfill.ts),
// so this is the per-request cap, not the per-reveal cost.
const SCROLLBACK_CELLS_MAX_ROWS = 2000;
// Rows per event-loop slice of that walk. Every OTHER session's PTY output on
// this worker is blocked for one slice, so keep it well under a frame. Matches
// the SPA's per-frame splice budget (scrollbackBackfill BACKFILL_SPLICE_ROWS).
const SCROLLBACK_CELLS_SLICE_ROWS = 250;

/** Demand-driven history from a stable grid epoch. Browser callers name the
 * authoritative viewport frame they hold. An empty epoch is the headless-API
 * form: bind this read to the worker's current epoch and return that identity.
 * The epoch is checked after every yield in either form, so neither can splice
 * rows across a reframe. */
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
	// A dims-change claim rebuilds a fresh core inside its terminal-control
	// transaction; serving mid-rebuild would hand out rows the imminent reframe
	// invalidates. The lane tail never rejects — a failed transaction reports
	// itself and leaves the current core serveable.
	if (sessionMgr.terminalControlChains.has(rec.channelId)) {
		await terminalControlSettled(sessionMgr, rec.channelId);
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
	const requestedEpoch = frame.grid_epoch;
	const currentEpoch = cellGridEpoch(rec.cell_emit);
	const expectedEpoch = requestedEpoch || currentEpoch;
	if (requestedEpoch && requestedEpoch !== currentEpoch) {
		coordLink.send({ kind: "rpc-error", request_id, message: "grid epoch changed" });
		return;
	}
	try {
		// Monotonic index space (grid-to-cells.ts): the SPA's row indices are
		// sbDropped-based, so clamp the request into [sbDropped, sbDropped+count].
		// A request below sbDropped names rows the core no longer holds. A short page
		// names the surviving suffix and exposes its first absolute index as the
		// retained floor; `history_floor` says WHY the rest is gone — genuine
		// eviction, or a resize-forced replay bounded by the byte ring that a
		// never-resized session would have survived — so the SPA stops paging AND can
		// name the floor it shows instead of a bare blank gap.
		//
		// Read the origin from the CORE, not the last emitted frame: the ring keeps
		// evicting between emits, and a stale origin shifts every offset these
		// absolute indices resolve through — real rows under the wrong indices,
		// which is worse than the short page.
		const sbDropped = scrollbackOrigin(core, rec.cell_emit);
		const total = sbDropped + core.getScrollbackCount();
		const endRow = Math.min(frame.end_row, total);
		const wantStart = endRow - Math.min(frame.max_rows, SCROLLBACK_CELLS_MAX_ROWS);
		const startRow = Math.max(sbDropped, wantStart);
		const historyFloor = historyFloorReason(rec, wantStart, sbDropped);
		// Sliced walk: 999 rows × cols is ~120k WASM cell reads on an 80-col
		// grid, and step-3's wave lands three of these back to back. Yield
		// between slices, then re-validate the grid identity — a reframe or an
		// eviction past our start shifts the offsets our absolute indices
		// resolve through, so abort rather than return a hole. The browser parks
		// this demand attempt; a later explicit scroll/find may retry against the
		// next authoritative frame.
		const rows: CellRow[] = [];
		let liveDropped = sbDropped;
		let slices = 0;
		for (let sliceStart = startRow; sliceStart < endRow; sliceStart += SCROLLBACK_CELLS_SLICE_ROWS) {
			if (slices > 0) {
				await new Promise<void>((resolve) => { setImmediate(resolve); });
				const liveRec = sessionMgr.getBySessionId(frame.session_id);
				if (!liveRec || expectedEpoch !== cellGridEpoch(liveRec.cell_emit)) {
					coordLink.send({ kind: "rpc-error", request_id, message: "grid epoch changed" });
					return;
				}
				rec = liveRec;
				if (rec.wtermCore !== core) {
					coordLink.send({ kind: "rpc-error", request_id, message: "grid reframed mid-read" });
					return;
				}
				liveDropped = scrollbackOrigin(core, rec.cell_emit);
				if (liveDropped > startRow) {
					coordLink.send({ kind: "rpc-error", request_id, message: "scrollback evicted mid-read" });
					return;
				}
			}
			const sliceEnd = Math.min(sliceStart + SCROLLBACK_CELLS_SLICE_ROWS, endRow);
			for (const row of readScrollbackRangeCells(core, sliceStart, sliceEnd, liveDropped)) rows.push(row);
			slices++;
		}
		diag("scrollback.cells", {
			sid: rec.sessionId,
			channel_id: rec.channelId,
			session_trace_id: rec.session_trace_id,
			start_row: startRow, end_row: endRow, want_start: wantStart,
			total, sb_dropped: sbDropped, history_floor: historyFloor,
			rows: rows.length,
			slices,
		});
		coordLink.send({
			kind: "rpc-ok",
			request_id,
			data: {
				rows, cols: core.getCols(), total, start_row: startRow, end_row: endRow,
				grid_epoch: expectedEpoch, history_floor: historyFloor,
			},
		});
	} catch (err) {
		coordLink.send({
			kind: "rpc-error",
			request_id,
			message: err instanceof Error ? err.message : String(err),
		});
	}
}

