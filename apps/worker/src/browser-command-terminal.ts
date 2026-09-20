// Bounded, demand-driven scrollback cell retrieval from the worker's
// authoritative grid. readScrollbackCells is the ONE reader: the coordinator
// RPC below and the local terminal socket both wrap it, so neither transport
// owns a second traversal of the cell codec or a second set of grid-epoch and
// terminal-control fences.

import type { ClientControlFrame, ScrollbackHistoryFloor } from "@roost/shared/wire";
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

export interface ScrollbackCellsRequest {
	sessionId: string;
	/** Empty binds the read to the worker's current epoch and returns it. */
	gridEpoch: string;
	endRow: number;
	maxRows: number;
	/** Optional transport budget gate, evaluated before retaining each row. */
	admitRow?: (row: CellRow) => boolean;
	/** Live transport authority; direct reads stop after close/revoke between slices. */
	continueRead?: () => boolean;
}

export interface ScrollbackCellsPage {
	rows: CellRow[];
	cols: number;
	total: number;
	startRow: number;
	endRow: number;
	gridEpoch: string;
	historyFloor: ScrollbackHistoryFloor;
}

export type ScrollbackCellsResult =
	| { ok: true; page: ScrollbackCellsPage }
	| { ok: false; error: string };

/** Demand-driven history from a stable grid epoch. Browser callers name the
 * authoritative viewport frame they hold. An empty epoch is the headless-API
 * form: bind this read to the worker's current epoch and return that identity.
 * The epoch is checked after every yield in either form, so neither can splice
 * rows across a reframe. */
export async function readScrollbackCells(
	sessionMgr: SessionManager,
	request: ScrollbackCellsRequest,
): Promise<ScrollbackCellsResult> {
	let rec = sessionMgr.getBySessionId(request.sessionId);
	if (!rec) return { ok: false, error: "unknown session" };
	// A dims-change claim rebuilds a fresh core inside its terminal-control
	// transaction; serving mid-rebuild would hand out rows the imminent reframe
	// invalidates. The lane tail never rejects — a failed transaction reports
	// itself and leaves the current core serveable.
	if (sessionMgr.terminalControlChains.has(rec.channelId)) {
		await terminalControlSettled(sessionMgr, rec.channelId);
		rec = sessionMgr.getBySessionId(request.sessionId);
		if (!rec) return { ok: false, error: "session closed" };
	}
	const core = rec.wtermCore;
	// Registered sessions own a terminal core. Keep the narrow for teardown
	// races and sparse test fixtures.
	if (!core) return { ok: false, error: "session has no terminal" };
	const requestedEpoch = request.gridEpoch;
	const currentEpoch = cellGridEpoch(rec.cell_emit);
	const expectedEpoch = requestedEpoch || currentEpoch;
	if (requestedEpoch && requestedEpoch !== currentEpoch) {
		return { ok: false, error: "grid epoch changed" };
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
		const endRow = Math.min(request.endRow, total);
		const wantStart = endRow - Math.min(request.maxRows, SCROLLBACK_CELLS_MAX_ROWS);
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
		let admittedRowCount = 0;
		const sliceRows = request.admitRow ? 1 : SCROLLBACK_CELLS_SLICE_ROWS;
		for (let sliceStart = startRow; sliceStart < endRow; sliceStart += sliceRows) {
			if (request.continueRead && !request.continueRead()) {
				return { ok: false, error: "terminal session is unavailable" };
			}
			if (slices > 0) {
				await new Promise<void>((resolve) => { setImmediate(resolve); });
				const liveRec = sessionMgr.getBySessionId(request.sessionId);
				if (!liveRec || expectedEpoch !== cellGridEpoch(liveRec.cell_emit)) {
					return { ok: false, error: "grid epoch changed" };
				}
				rec = liveRec;
				if (rec.wtermCore !== core) return { ok: false, error: "grid reframed mid-read" };
				liveDropped = scrollbackOrigin(core, rec.cell_emit);
				if (liveDropped > startRow) {
					return { ok: false, error: "scrollback evicted mid-read" };
				}
			}
			const sliceEnd = Math.min(sliceStart + sliceRows, endRow);
			for (const row of readScrollbackRangeCells(core, sliceStart, sliceEnd, liveDropped)) {
				if (request.admitRow) {
					if (!request.admitRow(row)) {
						return { ok: false, error: "scrollback response exceeds direct transport limit" };
					}
				} else {
					rows.push(row);
				}
				admittedRowCount++;
			}
			slices++;
		}
		diag("scrollback.cells", {
			sid: rec.sessionId,
			channel_id: rec.channelId,
			session_trace_id: rec.session_trace_id,
			start_row: startRow, end_row: endRow, want_start: wantStart,
			total, sb_dropped: sbDropped, history_floor: historyFloor,
			rows: admittedRowCount,
			slices,
		});
		return {
			ok: true,
			page: {
				rows, cols: core.getCols(), total, startRow, endRow,
				gridEpoch: expectedEpoch, historyFloor,
			},
		};
	} catch (err) {
		return { ok: false, error: err instanceof Error ? err.message : String(err) };
	}
}

export async function handleGetScrollbackCells(
	frame: Extract<ClientControlFrame, { kind: "get-scrollback-cells" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const result = await readScrollbackCells(deps.sessionMgr, {
		sessionId: frame.session_id,
		gridEpoch: frame.grid_epoch,
		endRow: frame.end_row,
		maxRows: frame.max_rows,
	});
	if (!result.ok) {
		deps.coordLink.send({ kind: "rpc-error", request_id, message: result.error });
		return;
	}
	const page = result.page;
	deps.coordLink.send({
		kind: "rpc-ok",
		request_id,
		data: {
			rows: page.rows, cols: page.cols, total: page.total,
			start_row: page.startRow, end_row: page.endRow,
			grid_epoch: page.gridEpoch, history_floor: page.historyFloor,
		},
	});
}
