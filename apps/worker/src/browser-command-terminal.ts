// Browser-command handlers: terminal viewport + scrollback reads
// (get-scrollback-cells / search-scrollback / resize). Extracted from
// browser-command-handler.ts (CLAUDE.md 400-line cap).

import type { ClientControlFrame } from "@roost/shared/wire";
import { diag } from "@roost/shared";
import { cellGridEpoch, readScrollbackRangeCells, type CellRow } from "@roost/shared/cell";
import type { CoordLink } from "./transport/CoordLink.ts";
import type { SessionManager } from "./session-manager.ts";
import type { TerminalCore } from "@wterm/core";

// Server-side ceiling on rows per get-scrollback-cells response — bounds the
// per-cell WASM walk (and the rpc-ok JSON) for one RPC. The SPA chunks at 1000
// and issues BACKFILL_CONCURRENCY of those per wave (scrollbackBackfill.ts),
// so this is the per-request cap, not the per-reveal cost.
const SCROLLBACK_CELLS_MAX_ROWS = 2000;
// Rows per event-loop slice of that walk. Every OTHER session's PTY output on
// this worker is blocked for one slice, so keep it well under a frame. Matches
// the SPA's per-frame splice budget (scrollbackBackfill BACKFILL_SPLICE_ROWS).
const SCROLLBACK_CELLS_SLICE_ROWS = 250;

// Find-in-scrollback bounds. The SPA holds at most 2000 of the worker's 10k
// retained rows, so this walk is the only complete search — but it is also
// ~800k WASM reads at full depth, which is why it is sliced and bounded.
const SEARCH_MAX_MATCHES = 500;
// Under coord's createPendingRpc(8_000) so a deadline hit still answers.
const SEARCH_DEADLINE_MS = 5_000;
// Rows per event-loop slice. Every OTHER session's PTY output on this worker
// is blocked for the duration of one slice, so keep it well under a frame.
const SEARCH_SLICE_ROWS = 500;
// Enough for a result-list entry; the SPA jumps to the row for the rest.
const SEARCH_PREVIEW_CHARS = 200;

interface RowMatch { col: number; len: number }
const NO_MATCHES: readonly RowMatch[] = [];

function _charOf(codepoint: number): string {
	// wterm leaves NUL in a never-touched cell; it paints as a space.
	if (codepoint === 0) return " ";
	try { return String.fromCodePoint(codepoint); } catch { return " "; }
}

function _viewportRowText(core: TerminalCore, row: number, cols: number): string {
	let text = "";
	for (let col = 0; col < cols; col++) text += _charOf(core.getCell(row, col).char);
	return text;
}

function _scrollbackRowText(core: TerminalCore, offset: number): string {
	const len = core.getScrollbackLineLen(offset);
	let text = "";
	for (let col = 0; col < len; col++) text += _charOf(core.getScrollbackCell(offset, col).char);
	return text;
}

/** `needle` must already be lowercased when the search is case-insensitive. */
function _plainRowMatches(text: string, needle: string, caseSensitive: boolean): readonly RowMatch[] {
	const haystack = caseSensitive ? text : text.toLowerCase();
	let at = haystack.indexOf(needle);
	if (at < 0) return NO_MATCHES;
	const out: RowMatch[] = [];
	while (at >= 0) {
		out.push({ col: at, len: needle.length });
		at = haystack.indexOf(needle, at + needle.length);
	}
	return out;
}

function _regexRowMatches(text: string, re: RegExp): readonly RowMatch[] {
	re.lastIndex = 0;
	let hit = re.exec(text);
	if (hit === null) return NO_MATCHES;
	const out: RowMatch[] = [];
	while (hit !== null) {
		out.push({ col: hit.index, len: hit[0].length });
		// A zero-width pattern (`x*`) leaves lastIndex where it was — without
		// this nudge exec() returns the same empty match forever.
		if (hit[0].length === 0) re.lastIndex++;
		hit = re.exec(text);
	}
	return out;
}

/** Demand-driven history from the immutable grid epoch named by the browser's
 * authoritative viewport frame. The epoch is checked before any cell walk and
 * after every event-loop yield, so a reframe can never splice re-numbered rows. */
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
	if (frame.grid_epoch !== cellGridEpoch(rec.cell_emit)) {
		coordLink.send({ kind: "rpc-error", request_id, message: "grid epoch changed" });
		return;
	}
	try {
		// Monotonic index space (grid-to-cells.ts): the SPA's row indices are
		// sbDropped-based, so clamp the request into [sbDropped, sbDropped+count].
		// A request reaching below sbDropped names rows the ring genuinely
		// dropped. A short page names the surviving suffix and exposes its first
		// absolute index as the retained floor; the SPA paints that suffix and
		// stops paging once it reaches the floor.
		const sbDropped = rec.cell_emit.sbDropped;
		const total = sbDropped + core.getScrollbackCount();
		const endRow = Math.min(frame.end_row, total);
		const startRow = Math.max(sbDropped, endRow - Math.min(frame.max_rows, SCROLLBACK_CELLS_MAX_ROWS));
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
				if (!liveRec || frame.grid_epoch !== cellGridEpoch(liveRec.cell_emit)) {
					coordLink.send({ kind: "rpc-error", request_id, message: "grid epoch changed" });
					return;
				}
				rec = liveRec;
				if (rec.wtermCore !== core) {
					coordLink.send({ kind: "rpc-error", request_id, message: "grid reframed mid-read" });
					return;
				}
				liveDropped = rec.cell_emit.sbDropped;
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
			start_row: startRow,
			end_row: endRow,
			total,
			sb_dropped: sbDropped,
			rows: rows.length,
			slices,
		});
		coordLink.send({
			kind: "rpc-ok",
			request_id,
			data: {
				rows, cols: core.getCols(), total, start_row: startRow, end_row: endRow,
				grid_epoch: frame.grid_epoch,
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

// cell-phase-4: handleGetScrollbackSince retired — cell-mode scrollback backfill
// serves cell rows via handleGetScrollbackCells instead.

/** Find-in-scrollback: substring or regex over the worker's authoritative
 *  grid. The SPA holds at most a 2000-row window of the 10k rows the core
 *  retains, so a client-side find would silently miss most of a long session.
 *
 *  Traversal is newest-first — scrollback offset 0 (the line just above the
 *  viewport) through the oldest retained line, then the live viewport rows —
 *  so hitting SEARCH_MAX_MATCHES or SEARCH_DEADLINE_MS keeps the recent
 *  history a user is most likely after, reported as truncated:true.
 *
 *  Rows are named by MONOTONIC absolute index, the same space as
 *  PbCellRow.index and a cell frame's sbBase, so the SPA can jump its reader
 *  straight to `row`. The mapping is grid-to-cells.ts::_scrollbackRow inverted:
 *  offset = count - 1 - (index - sbDropped).
 *
 *  The scan yields the event loop every SEARCH_SLICE_ROWS rows — a full-depth
 *  walk is ~800k WASM reads, and holding the loop for it stalls every OTHER
 *  session's PTY on this worker. THIS session can print across those yields,
 *  so the ring origin is re-read per slice and the offset re-derived from the
 *  stable absolute index rather than carried across the await. */
export async function handleSearchScrollback(
	frame: Extract<ClientControlFrame, { kind: "search-scrollback" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	let rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "unknown session" });
		return;
	}
	// Same rebuild interlock as handleGetScrollbackCells: searching mid-replay
	// would name rows the imminent reframe invalidates.
	const pendingRebuild = sessionMgr._wtermRebuildChain.get(rec.channelId);
	if (pendingRebuild) {
		try {
			await pendingRebuild;
		} catch {
			// rebuild chain logs its own failures; search the current core
		}
		rec = sessionMgr.getBySessionId(frame.session_id);
		if (!rec) {
			coordLink.send({ kind: "rpc-error", request_id, message: "session closed" });
			return;
		}
	}
	const session = rec;
	const core = session.wtermCore;
	if (!core) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session has no terminal" });
		return;
	}

	let re: RegExp | null = null;
	if (frame.regex && frame.query.length > 0) {
		try {
			// Compiled once for the whole grid; lastIndex is reset per row.
			re = new RegExp(frame.query, frame.case_sensitive ? "g" : "gi");
		} catch (err) {
			coordLink.send({
				kind: "rpc-error",
				request_id,
				message: `invalid regex: ${err instanceof Error ? err.message : String(err)}`,
			});
			return;
		}
	}

	const cap = Math.min(frame.max_matches, SEARCH_MAX_MATCHES);
	const needle = frame.case_sensitive ? frame.query : frame.query.toLowerCase();
	const cols = core.getCols();
	let sbDropped = session.cell_emit.sbDropped;
	let sbCount = core.getScrollbackCount();
	const monoTotal = sbDropped + sbCount;
	const matches: Array<{ row: number; col: number; len: number; preview: string }> = [];
	let truncated = false;
	let scanned = 0;
	const deadline = Date.now() + SEARCH_DEADLINE_MS;

	const collectRow = (absIndex: number, text: string): void => {
		const hits = re !== null
			? _regexRowMatches(text, re)
			: _plainRowMatches(text, needle, frame.case_sensitive);
		if (hits.length === 0) return;
		const preview = text.trimEnd().slice(0, SEARCH_PREVIEW_CHARS);
		for (const hit of hits) {
			if (matches.length >= cap) {
				truncated = true;
				return;
			}
			matches.push({ row: absIndex, col: hit.col, len: hit.len, preview });
		}
	};

	/** Slice boundary. False = stop scanning (deadline, or the core was swapped
	 *  under us and its offsets no longer mean what our indices say). */
	const yieldSlice = async (): Promise<boolean> => {
		if (Date.now() >= deadline) {
			truncated = true;
			return false;
		}
		await new Promise<void>((resolve) => { setImmediate(resolve); });
		sbDropped = session.cell_emit.sbDropped;
		sbCount = core.getScrollbackCount();
		if (session.wtermCore !== core) {
			truncated = true;
			return false;
		}
		return true;
	};

	try {
		// An empty query matches every position and means nothing — never walk
		// the grid for it.
		if (frame.query.length > 0) {
			for (let absIndex = monoTotal - 1; absIndex >= sbDropped; absIndex--) {
				const offset = sbCount - 1 - (absIndex - sbDropped);
				// Evicted across a yield; everything older is gone too.
				if (offset < 0 || offset >= sbCount) break;
				collectRow(absIndex, _scrollbackRowText(core, offset));
				scanned++;
				if (truncated) break;
				if (scanned % SEARCH_SLICE_ROWS === 0 && !(await yieldSlice())) break;
			}
			if (!truncated) {
				// Live viewport rows sit above the whole ring; their base moves
				// with it, so read it after the scrollback walk's last refresh.
				const viewportBase = sbDropped + sbCount;
				const viewportRows = core.getRows();
				for (let row = 0; row < viewportRows; row++) {
					collectRow(viewportBase + row, _viewportRowText(core, row, cols));
					scanned++;
					if (truncated) break;
					if (scanned % SEARCH_SLICE_ROWS === 0 && !(await yieldSlice())) break;
				}
			}
		}
		diag("scrollback.search", {
			sid: session.sessionId,
			channel_id: session.channelId,
			session_trace_id: session.session_trace_id,
			// Length only — the query is user content and never leaves as text.
			query_len: frame.query.length,
			regex: frame.regex,
			case_sensitive: frame.case_sensitive,
			matches: matches.length,
			truncated,
			rows_scanned: scanned,
		});
		coordLink.send({
			kind: "rpc-ok",
			request_id,
			data: { matches, truncated, total: monoTotal, cols },
		});
	} catch (err) {
		coordLink.send({
			kind: "rpc-error",
			request_id,
			message: err instanceof Error ? err.message : String(err),
		});
	}
}

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
			frame.held_cell_seq,
		);
	return;
}
