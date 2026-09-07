// Bounded cursor search over one worker-owned terminal grid.
// The browser-command dispatcher calls it to traverse retained scrollback and
// the live viewport within one epoch and shared-schema result contract.

import {
	cellGridEpoch,
	scrollbackOffsetSpans,
	scrollbackOrigin,
	spansText,
	textRangeToColumns,
	viewportRowSpans,
	type CellSpan,
} from "@roost/shared/cell";
import {
	TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS,
	TERMINAL_SEARCH_RPC_DEADLINE_MS,
	truncateUnicodeCodePoints,
	type SearchStopReason,
	type WorkerSearchScrollbackMatch,
} from "@roost/shared/terminal-search";
import type { ClientControlFrame } from "@roost/shared/wire";
import type { SessionManager } from "./session-manager.ts";
import type { SessionShellRecord } from "./session-record.ts";
import { terminalControlSettled } from "./session-control-lanes.ts";
import {
	compileTerminalSearchExpression,
	visitPlainRowMatches,
	_visitRegexRowMatches,
	type RowMatchVisitor,
	type TerminalSearchExpression,
} from "./terminal-search-matcher.ts";
import {
	terminalControlSettlesBeforeSearchDeadline,
	terminalSearchEventLoopYield,
} from "./terminal-search-scheduling.ts";
import {
	cancelSearchScrollback,
	consumeSearchCancellation,
	searchOwnerKey,
} from "./terminal-search-cancellation.ts";
import {
	publishPreScanDeadline,
	publishSearchResult,
} from "./terminal-search-result.ts";
import { historyFloorReason } from "./session-scrollback.ts";
import type { CoordLink } from "./transport/coord-link.ts";
import { monoNowMs } from "./util/mono.ts";

// Schema validation and outbound transport share the outer eight seconds; keep
// a fixed final half-second for result construction, queuing, and return travel.
const SEARCH_RETURN_RESERVE_MS = 500;
const SEARCH_WORK_DEADLINE_MS = TERMINAL_SEARCH_RPC_DEADLINE_MS - SEARCH_RETURN_RESERVE_MS;
const SEARCH_MAX_ACTIVE = 8;
// One slice bounds how long this scan can withhold every other PTY on the worker.
const SEARCH_SLICE_ROWS = 500;

type SearchFrame = Extract<ClientControlFrame, { kind: "search-scrollback" }>;
type SearchDeps = { coordLink: CoordLink; sessionMgr: SessionManager; searchOwnerId: string };

export { cancelSearchScrollback };

/** Injectable scheduling seams keep deadline and epoch-boundary tests exact. */
export interface _SearchScrollbackRuntime {
	nowMs: () => number;
	yieldNow: () => Promise<void>;
	waitForTerminalControl?: (
		settled: Promise<void>,
		remainingMs: number,
		signal: AbortSignal,
	) => Promise<boolean>;
	deadlineAtMs?: number;
}

function rowSpans(
	session: SessionShellRecord,
	absoluteRow: number,
	floor: number,
	scrollbackCount: number,
	cols: number,
): readonly CellSpan[] | null {
	if (absoluteRow < floor) return null;
	const scrollbackTotal = floor + scrollbackCount;
	if (absoluteRow < scrollbackTotal) {
		const offset = scrollbackCount - 1 - (absoluteRow - floor);
		return scrollbackOffsetSpans(session.wtermCore, offset);
	}
	const viewportRow = absoluteRow - scrollbackTotal;
	if (viewportRow < 0 || viewportRow >= session.wtermCore.getRows()) return null;
	return viewportRowSpans(session.wtermCore, viewportRow, cols);
}



/** Search newest-to-oldest in the same absolute row numbering used by cell
 * frames. `before_row` is exclusive, every reported scan range is half-open,
 * and a row- or match-limit stop continues at that range's start. */
export async function handleSearchScrollback(
	frame: SearchFrame,
	requestId: string,
	deps: SearchDeps,
	runtime?: _SearchScrollbackRuntime,
): Promise<void> {
	if (consumeSearchCancellation(frame, deps.searchOwnerId, deps.sessionMgr)) {
		deps.coordLink.send({
			kind: "rpc-error",
			request_id: requestId,
			message: "scrollback search superseded",
		});
		return;
	}
	const session = deps.sessionMgr.getBySessionId(frame.session_id);
	if (!session) {
		deps.coordLink.send({ kind: "rpc-error", request_id: requestId, message: "unknown session" });
		return;
	}
	const searchKey = searchOwnerKey(session.channelId, deps.searchOwnerId);
	const activeSearch = deps.sessionMgr.terminalSearches.get(searchKey);
	if (!activeSearch && deps.sessionMgr.terminalSearches.size >= SEARCH_MAX_ACTIVE) {
		deps.coordLink.send({
			kind: "rpc-error",
			request_id: requestId,
			message: "too many active scrollback searches",
		});
		return;
	}
	activeSearch?.controller.abort();
	const search = {
		searchId: frame.search_id,
		controller: new AbortController(),
	};
	deps.sessionMgr.terminalSearches.set(searchKey, search);
	try {
		await searchScrollbackAdmitted(
			frame,
			requestId,
			deps,
			session,
			search.controller.signal,
			runtime,
		);
	} finally {
		if (deps.sessionMgr.terminalSearches.get(searchKey) === search) {
			deps.sessionMgr.terminalSearches.delete(searchKey);
		}
	}
}

async function searchScrollbackAdmitted(
	frame: SearchFrame,
	requestId: string,
	deps: SearchDeps,
	initialSession: SessionShellRecord,
	signal: AbortSignal,
	runtime?: _SearchScrollbackRuntime,
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const nowMs = runtime?.nowMs ?? monoNowMs;
	const yieldNow = runtime?.yieldNow ?? terminalSearchEventLoopYield;
	const waitForTerminalControl = runtime?.waitForTerminalControl
		?? terminalControlSettlesBeforeSearchDeadline;
	const deadlineAt = runtime?.deadlineAtMs ?? nowMs() + SEARCH_WORK_DEADLINE_MS;
	let session = initialSession;
	if (sessionMgr.terminalControlChains.has(session.channelId)) {
		const settled = terminalControlSettled(sessionMgr, session.channelId);
		if (!(await waitForTerminalControl(settled, deadlineAt - nowMs(), signal))) {
			if (signal.aborted) {
				coordLink.send({ kind: "rpc-error", request_id: requestId, message: "scrollback search superseded" });
				return;
			}
			const liveSession = sessionMgr.getBySessionId(frame.session_id);
			if (!liveSession) {
				coordLink.send({ kind: "rpc-error", request_id: requestId, message: "session closed" });
				return;
			}
			publishPreScanDeadline(frame, requestId, coordLink, liveSession);
			return;
		}
		const reboundSession = sessionMgr.getBySessionId(frame.session_id);
		if (!reboundSession) {
			coordLink.send({ kind: "rpc-error", request_id: requestId, message: "session closed" });
			return;
		}
		session = reboundSession;
	}

	const core = session.wtermCore;
	if (!core) {
		coordLink.send({ kind: "rpc-error", request_id: requestId, message: "session has no terminal" });
		return;
	}
	const currentEpoch = cellGridEpoch(session.cell_emit);
	const servingEpoch = frame.grid_epoch || currentEpoch;
	const cols = core.getCols();
	let liveFloor = scrollbackOrigin(core, session.cell_emit);
	let liveScrollbackCount = core.getScrollbackCount();
	const scrollbackTotal = liveFloor + liveScrollbackCount;
	const newestExclusive = scrollbackTotal + core.getRows();
	let scannedEndRow = Math.min(frame.before_row ?? newestExclusive, newestExclusive);
	let scannedStartRow = scannedEndRow;
	let matches: WorkerSearchScrollbackMatch[] = [];
	let rowsScanned = 0;
	let stopReason: SearchStopReason | null = null;
	let suppressedBoundaryMatches = false;

	// A page ending AT a nonzero floor must still name why no predecessor
	// exists, so classify the first unreachable row rather than the floor.
	const sendResult = (reason: SearchStopReason, gridEpoch: string): void => {
		const historyFloor = scannedStartRow <= liveFloor
			? historyFloorReason(session, liveFloor > 0 ? liveFloor - 1 : 0, liveFloor)
			: "none";
		// A match cap stops short of rows the ring still holds, so it hands back
		// the same cursor a row cap does — otherwise older matches are
		// unreachable. Omitted once the scan has reached the floor: no page left.
		const continues = reason === "row_limit"
			|| (reason === "match_limit" && scannedStartRow > liveFloor);
		publishSearchResult(frame, requestId, coordLink, session, {
			matches,
			truncated: reason === "match_limit" || reason === "deadline",
			scrollback_total: scrollbackTotal,
			cols,
			grid_epoch: gridEpoch,
			scanned_start_row: scannedStartRow,
			scanned_end_row: scannedEndRow,
			history_floor: historyFloor,
			...(continues ? { next_before_row: scannedStartRow } : {}),
			stop_reason: reason,
		});
	};
	if (frame.grid_epoch && frame.grid_epoch !== currentEpoch) {
		sendResult("epoch_changed", currentEpoch);
		return;
	}
	if (frame.query.length === 0) {
		scannedStartRow = 0;
		scannedEndRow = 0;
		sendResult("complete", servingEpoch);
		return;
	}

	let expression: TerminalSearchExpression | null = null;
	try {
		expression = compileTerminalSearchExpression(
			frame.query,
			frame.regex,
			frame.case_sensitive,
		);
	} catch {
		coordLink.send({
			kind: "rpc-error",
			request_id: requestId,
			message: "invalid regex: pattern could not be compiled",
		});
		return;
	}
	const needle = frame.query;

	try {
		let nextRow = scannedEndRow - 1;
		if (nextRow < liveFloor) stopReason = "complete";
		else if (nowMs() >= deadlineAt) stopReason = "deadline";
		while (stopReason === null && nextRow >= liveFloor) {
			if (nowMs() >= deadlineAt) {
				stopReason = "deadline";
				break;
			}
			const spans = rowSpans(session, nextRow, liveFloor, liveScrollbackCount, cols);
			if (spans === null) {
				if (nextRow < liveFloor) {
					stopReason = "complete";
					break;
				}
				throw new Error("scrollback search row unavailable");
			}
			const text = spansText(spans);
			let preview: string | undefined;
			const collectMatch: RowMatchVisitor = (offset, length) => {
				if (matches.length >= frame.max_matches) {
					suppressedBoundaryMatches = true;
					return;
				}
				preview ??= truncateUnicodeCodePoints(text.trimEnd(), TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS);
				const range = textRangeToColumns(spans, offset, length);
				matches.push({
					row: nextRow,
					col: range.col,
					len: range.columns,
					preview,
				});
			};
			if (expression === null) {
				visitPlainRowMatches(text, needle, collectMatch);
			} else {
				_visitRegexRowMatches(text, expression, collectMatch);
			}
			scannedStartRow = nextRow;
			rowsScanned++;
			nextRow--;

			const exhausted = nextRow < liveFloor;
			if (matches.length >= frame.max_matches && (!exhausted || suppressedBoundaryMatches)) {
				stopReason = "match_limit";
				break;
			}
			if (exhausted) {
				stopReason = "complete";
				break;
			}
			if (rowsScanned >= frame.max_rows) {
				stopReason = "row_limit";
				break;
			}
			if (nowMs() >= deadlineAt) {
				stopReason = "deadline";
				break;
			}
			if (rowsScanned % SEARCH_SLICE_ROWS !== 0) continue;

			await yieldNow();
			if (signal.aborted) {
				coordLink.send({ kind: "rpc-error", request_id: requestId, message: "scrollback search superseded" });
				return;
			}
			if (sessionMgr.terminalControlChains.has(session.channelId)) {
				const settled = terminalControlSettled(sessionMgr, session.channelId);
				if (!(await waitForTerminalControl(settled, deadlineAt - nowMs(), signal))) {
					if (signal.aborted) {
						coordLink.send({ kind: "rpc-error", request_id: requestId, message: "scrollback search superseded" });
						return;
					}
					stopReason = "deadline";
					break;
				}
			}
			const liveSession = sessionMgr.getBySessionId(frame.session_id);
			if (!liveSession) {
				coordLink.send({ kind: "rpc-error", request_id: requestId, message: "session closed" });
				return;
			}
			if (liveSession !== session || liveSession.wtermCore !== core
				|| cellGridEpoch(liveSession.cell_emit) !== servingEpoch) {
				stopReason = "epoch_changed";
				break;
			}
			liveFloor = scrollbackOrigin(core, session.cell_emit);
			liveScrollbackCount = core.getScrollbackCount();
			if (liveFloor > scannedStartRow) {
				matches = matches.filter((match) => match.row >= liveFloor);
				scannedStartRow = Math.min(scannedEndRow, liveFloor);
			}
			if (nextRow < liveFloor) {
				stopReason = "complete";
				break;
			}
			if (nowMs() >= deadlineAt) {
				stopReason = "deadline";
				break;
			}
		}
		sendResult(stopReason ?? "complete", servingEpoch);
	} catch (error) {
		coordLink.send({
			kind: "rpc-error",
			request_id: requestId,
			message: error instanceof Error ? error.message : String(error),
		});
	}
}
