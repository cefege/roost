// Coordinates one bounded terminal-content search across worker-local sessions.
// Browser command dispatch calls this owner once per coordinator batch; it
// reuses the single-session scanner while preserving one outer RPC reply.

import {
	allocateGlobalSearchMatchLimits,
	GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS,
	WorkerGlobalSearchResultSchema,
	WorkerSearchScrollbackResultSchema,
	type WorkerGlobalSearchEntry,
	type WorkerGlobalSearchError,
} from "@roost/shared/terminal-search";
import { log } from "@roost/shared/log";
import type { ClientControlFrame, SessionId } from "@roost/shared/wire";
import type { SessionManager } from "./session-manager.ts";
import {
	cancelSearchScrollback,
	handleSearchScrollback,
	type _SearchScrollbackRuntime,
} from "./terminal-search.ts";
import { terminalSearchEventLoopYield } from "./terminal-search-scheduling.ts";
import type { CoordLink, UpstreamFrame } from "./transport/coord-link-types.ts";
import { monoNowMs } from "./util/mono.ts";

type BatchSearchFrame = Extract<ClientControlFrame, { kind: "search-scrollback-batch" }>;
type BatchCancelFrame = Extract<ClientControlFrame, { kind: "cancel-scrollback-search-batch" }>;
type SessionSearchFrame = Extract<ClientControlFrame, { kind: "search-scrollback" }>;
type SearchRpcReply = Extract<UpstreamFrame, { kind: "rpc-ok" | "rpc-error" }>;
type BatchSearchDeps = {
	coordLink: CoordLink;
	sessionMgr: SessionManager;
	searchOwnerId: string;
};

/** Injectable seams keep batch scheduling and inter-request races deterministic. */
export interface _SearchScrollbackBatchRuntime {
	nowMs: () => number;
	yieldNow: () => Promise<void>;
	waitForTerminalControl?: _SearchScrollbackRuntime["waitForTerminalControl"];
	searchSession?: typeof handleSearchScrollback;
}

function errorEntry(
	sessionId: SessionId,
	error: WorkerGlobalSearchError,
): WorkerGlobalSearchEntry {
	return { status: "error", session_id: sessionId, error };
}

/** Test-visible normalization keeps teardown distinct from ordinary supersession. */
export function _mapGlobalSearchError(
	message: string,
	sessionExists = true,
): WorkerGlobalSearchError {
	if (message === "scrollback search superseded" && !sessionExists) {
		return "session_closed";
	}
	switch (message) {
		case "unknown session":
		case "session closed":
			return "session_closed";
		case "session has no terminal":
			return "no_terminal";
		case "scrollback search superseded":
		case "too many active scrollback searches":
			return "deadline";
		default:
			return "internal";
	}
}


function uniqueSessionIds(...groups: readonly (readonly SessionId[])[]): SessionId[] {
	const seen = new Set<SessionId>();
	const result: SessionId[] = [];
	for (const group of groups) {
		for (const sessionId of group) {
			if (seen.has(sessionId)) continue;
			seen.add(sessionId);
			result.push(sessionId);
		}
	}
	return result;
}

function cancelSessionSearches(
	searchId: string,
	sessionIds: readonly SessionId[],
	requestId: string,
	searchOwnerId: string,
	sessionMgr: SessionManager,
): void {
	for (const sessionId of sessionIds) {
		cancelSearchScrollback({
			kind: "cancel-scrollback-search",
			request_id: requestId,
			session_id: sessionId,
			search_request_id: searchId,
		}, searchOwnerId, sessionMgr);
	}
}

function retireActiveBatch(
	requestId: string,
	searchOwnerId: string,
	sessionMgr: SessionManager,
): void {
	const active = sessionMgr.terminalSearchBatches.get(searchOwnerId);
	if (!active) return;
	log.info("terminal-search-batch", "superseded", {
		request_id: requestId.slice(0, 128),
		search_id: active.searchId,
		owner_id: searchOwnerId.slice(0, 128),
		session_count: active.sessionIds.length,
	});
	cancelSessionSearches(
		active.searchId,
		active.sessionIds,
		requestId,
		searchOwnerId,
		sessionMgr,
	);
	if (sessionMgr.terminalSearchBatches.get(searchOwnerId) === active) {
		sessionMgr.terminalSearchBatches.delete(searchOwnerId);
	}
}

export function cancelSearchScrollbackBatch(
	frame: BatchCancelFrame,
	searchOwnerId: string,
	sessionMgr: SessionManager,
): void {
	const active = sessionMgr.terminalSearchBatches.get(searchOwnerId);
	const activeSessionIds = active?.searchId === frame.search_id
		? active.sessionIds
		: [];
	const canceledSessionIds = uniqueSessionIds(frame.session_ids, activeSessionIds);
	if (active?.searchId === frame.search_id) {
		log.info("terminal-search-batch", "canceled", {
			request_id: frame.request_id.slice(0, 128),
			search_id: frame.search_id,
			owner_id: searchOwnerId.slice(0, 128),
			session_count: canceledSessionIds.length,
		});
	}
	cancelSessionSearches(
		frame.search_id,
		canceledSessionIds,
		frame.request_id,
		searchOwnerId,
		sessionMgr,
	);
	if (
		active?.searchId === frame.search_id
		&& sessionMgr.terminalSearchBatches.get(searchOwnerId) === active
	) {
		sessionMgr.terminalSearchBatches.delete(searchOwnerId);
	}
}

async function searchOneBatchSession(
	frame: BatchSearchFrame,
	sessionIndex: number,
	matchLimit: number,
	requestId: string,
	deps: BatchSearchDeps,
	searchRuntime: _SearchScrollbackRuntime,
	searchSession: typeof handleSearchScrollback,
): Promise<WorkerGlobalSearchEntry> {
	const cursor = frame.sessions[sessionIndex]!;
	const innerRequestId = `${requestId}:${sessionIndex}`;
	const replies: SearchRpcReply[] = [];
	let malformedReply = false;
	const captureLink = {
		send(reply: UpstreamFrame): boolean {
			if (reply.kind === "rpc-ok" || reply.kind === "rpc-error") replies.push(reply);
			else malformedReply = true;
			return true;
		},
	} as CoordLink;
	const sessionFrame: SessionSearchFrame = {
		kind: "search-scrollback",
		request_id: innerRequestId,
		session_id: cursor.session_id,
		search_id: frame.search_id,
		grid_epoch: cursor.grid_epoch,
		query: frame.query,
		case_sensitive: frame.case_sensitive,
		regex: false,
		...(cursor.before_row === undefined ? {} : { before_row: cursor.before_row }),
		max_rows: frame.max_rows_per_session,
		max_matches: matchLimit,
	};
	try {
		await searchSession(sessionFrame, innerRequestId, {
			coordLink: captureLink,
			sessionMgr: deps.sessionMgr,
			searchOwnerId: deps.searchOwnerId,
		}, searchRuntime);
	} catch {
		return errorEntry(cursor.session_id, "internal");
	}
	if (malformedReply || replies.length !== 1) {
		return errorEntry(cursor.session_id, "internal");
	}
	const reply = replies[0]!;
	if (reply.request_id !== innerRequestId) {
		return errorEntry(cursor.session_id, "internal");
	}
	if (reply.kind === "rpc-error") {
		return errorEntry(
			cursor.session_id,
			_mapGlobalSearchError(
				reply.message,
				Boolean(deps.sessionMgr.getBySessionId(cursor.session_id)),
			),
		);
	}
	const parsed = WorkerSearchScrollbackResultSchema.safeParse(reply.data);
	if (!parsed.success || parsed.data.matches.length > matchLimit) {
		return errorEntry(cursor.session_id, "internal");
	}
	return { status: "ok", session_id: cursor.session_id, result: parsed.data };
}

export async function handleSearchScrollbackBatch(
	frame: BatchSearchFrame,
	requestId: string,
	deps: BatchSearchDeps,
	runtime?: _SearchScrollbackBatchRuntime,
): Promise<void> {
	const nowMs = runtime?.nowMs ?? monoNowMs;
	const deadlineAtMs = nowMs() + GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS;
	retireActiveBatch(requestId, deps.searchOwnerId, deps.sessionMgr);
	const activeBatch = {
		searchId: frame.search_id,
		sessionIds: frame.sessions.map(cursor => cursor.session_id),
	};
	deps.sessionMgr.terminalSearchBatches.set(deps.searchOwnerId, activeBatch);
	log.info("terminal-search-batch", "admitted", {
		request_id: requestId.slice(0, 128),
		search_id: frame.search_id,
		owner_id: deps.searchOwnerId.slice(0, 128),
		session_count: frame.sessions.length,
	});
	const matchLimits = allocateGlobalSearchMatchLimits(frame.max_matches, frame.sessions.length);
	const entries: WorkerGlobalSearchEntry[] = [];
	const searchRuntime: _SearchScrollbackRuntime = {
		nowMs,
		yieldNow: runtime?.yieldNow ?? terminalSearchEventLoopYield,
		...(runtime?.waitForTerminalControl === undefined
			? {}
			: { waitForTerminalControl: runtime.waitForTerminalControl }),
		deadlineAtMs,
	};
	const searchSession = runtime?.searchSession ?? handleSearchScrollback;

	try {
		for (let index = 0; index < frame.sessions.length; index++) {
			const cursor = frame.sessions[index]!;
			const matchLimit = matchLimits[index]!;
			if (
				deps.sessionMgr.terminalSearchBatches.get(deps.searchOwnerId) !== activeBatch
				|| nowMs() >= deadlineAtMs
				|| matchLimit === 0
			) {
				entries.push(errorEntry(cursor.session_id, "deadline"));
				continue;
			}
			entries.push(await searchOneBatchSession(
				frame,
				index,
				matchLimit,
				requestId,
				deps,
				searchRuntime,
				searchSession,
			));
			if (index + 1 < frame.sessions.length) await searchRuntime.yieldNow();
		}
	} catch {
		for (let index = entries.length; index < frame.sessions.length; index++) {
			entries.push(errorEntry(frame.sessions[index]!.session_id, "internal"));
		}
	} finally {
		if (deps.sessionMgr.terminalSearchBatches.get(deps.searchOwnerId) === activeBatch) {
			deps.sessionMgr.terminalSearchBatches.delete(deps.searchOwnerId);
		}
	}

	const parsed = WorkerGlobalSearchResultSchema.safeParse({ entries });
	const data = parsed.success
		? parsed.data
		: WorkerGlobalSearchResultSchema.parse({
			entries: frame.sessions.map(cursor => errorEntry(cursor.session_id, "internal")),
		});
	let matchCount = 0;
	let errorCount = 0;
	for (const entry of data.entries) {
		if (entry.status === "ok") matchCount += entry.result.matches.length;
		else errorCount++;
	}
	log.info("terminal-search-batch", "completed", {
		request_id: requestId.slice(0, 128),
		search_id: frame.search_id,
		owner_id: deps.searchOwnerId.slice(0, 128),
		session_count: data.entries.length,
		match_count: matchCount,
		error_count: errorCount,
	});
	deps.coordLink.send({ kind: "rpc-ok", request_id: requestId, data });
}
