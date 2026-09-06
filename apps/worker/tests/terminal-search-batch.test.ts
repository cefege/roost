// Worker batch scrollback-search tests pin aggregate budgeting, one-reply
// dispatch, shared deadlines, typed failures, cancellation tombstones, and
// latest-batch retirement across sessions owned by one stable viewer.

import { describe, expect, test } from "bun:test";
import { initCellEmitState } from "@roost/shared/cell";
import {
	GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
	GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
	GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS,
	WorkerGlobalSearchResultSchema,
	type WorkerGlobalSearchResult,
	type WorkerSearchScrollbackResult,
} from "@roost/shared/terminal-search";
import {
	asChannelId,
	asSessionId,
	asWorkerFp,
	type ClientControlFrame,
	type SessionId,
} from "@roost/shared/wire";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { handleBrowserCommand } from "../src/browser-command-handler.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import {
	cancelSearchScrollbackBatch,
	handleSearchScrollbackBatch,
	type _SearchScrollbackBatchRuntime,
} from "../src/terminal-search-batch.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import type { CoordLink, UpstreamFrame } from "../src/transport/coord-link-types.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const SESSION_IDS = [
	asSessionId("00000000-0000-0000-0000-000000000001"),
	asSessionId("00000000-0000-0000-0000-000000000002"),
	asSessionId("00000000-0000-0000-0000-000000000003"),
] as const;
const FIXED_RUNTIME: _SearchScrollbackBatchRuntime = {
	nowMs: () => 0,
	yieldNow: async () => {},
};
type BatchFrame = Extract<ClientControlFrame, { kind: "search-scrollback-batch" }>;
type CancelBatchFrame = Extract<ClientControlFrame, { kind: "cancel-scrollback-search-batch" }>;

function manager(): SessionManager {
	return new SessionManager({
		workerFp: asWorkerFp("00".repeat(32)),
		sink: new SessionEventTestSink(),
	});
}

async function injectSession(
	sessionMgr: SessionManager,
	sessionId: SessionId,
	channelNumber: number,
	text: string,
): Promise<void> {
	const bytes = new TextEncoder().encode(text);
	const wtermCore = await createWtermCore(80, 24);
	wtermCore.writeRaw(bytes);
	const record: SessionShellRecord = {
		sessionId,
		channelId: asChannelId(channelNumber),
		socketPath: "/dev/null",
		kind: "shell",
		cwd: "/",
		shellSpec: keeperTestShellSpec({ executable: process.execPath, cwd: "/" }),
		fsm: {} as FsmChannel,
		scrollback: createSbRing(new Uint8Array(bytes)),
		head_seq: bytes.length,
		alt_mode: false,
		mode_carry: new Uint8Array(0),
		osc7_carry: new Uint8Array(0),
		query_carry: new Uint8Array(0),
		...initAgentOscState(),
		wtermCore,
		session_trace_id: `batch${channelNumber}`,
		cell_emit: initCellEmitState(
			`batch-grid-${channelNumber}`,
			`00000000-0000-4000-8000-${String(channelNumber).padStart(12, "0")}`,
		),
		lastPtyOutMs: 0,
		sb_origin_pin: null,
		spawnedAtMs: Date.now(),
		closeReservation: sessionMgr.reserveSessionEvent("closed"),
	};
	sessionMgr.sessions.set(asChannelId(channelNumber), record);
}

function batchFrame(
	searchId: string,
	sessionIds: readonly SessionId[],
	options: { query?: string; maxMatches?: number } = {},
): BatchFrame {
	return {
		kind: "search-scrollback-batch",
		request_id: `inner-${searchId}`,
		search_id: searchId,
		query: options.query ?? "BATCH-MARKER",
		case_sensitive: false,
		sessions: sessionIds.map(sessionId => ({ session_id: sessionId, grid_epoch: "" })),
		max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
		max_matches: options.maxMatches ?? 256,
		deadline_ms: GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
	};
}

function cancelFrame(searchId: string, sessionIds: readonly SessionId[]): CancelBatchFrame {
	return {
		kind: "cancel-scrollback-search-batch",
		request_id: `cancel-${searchId}`,
		search_id: searchId,
		session_ids: [...sessionIds],
	};
}

function captureLink(): { coordLink: CoordLink; sent: UpstreamFrame[] } {
	const sent: UpstreamFrame[] = [];
	return {
		coordLink: {
			send: (frame: UpstreamFrame) => {
				sent.push(frame);
				return true;
			},
		} as CoordLink,
		sent,
	};
}

function resultFromOnlyReply(sent: UpstreamFrame[]): WorkerGlobalSearchResult {
	expect(sent).toHaveLength(1);
	const reply = sent[0]!;
	expect(reply.kind).toBe("rpc-ok");
	if (reply.kind !== "rpc-ok") throw new Error("expected batch rpc-ok");
	return WorkerGlobalSearchResultSchema.parse(reply.data);
}

function emptySearchResult(gridEpoch: string): WorkerSearchScrollbackResult {
	return {
		matches: [],
		truncated: false,
		total: 0,
		cols: 80,
		grid_epoch: gridEpoch,
		scanned_start_row: 0,
		scanned_end_row: 0,
		history_floor: "none",
		stop_reason: "complete",
	};
}

describe("worker scrollback-search batches", () => {
	test("two sessions share one outer reply in request order", async () => {
		const sessionMgr = manager();
		await injectSession(sessionMgr, SESSION_IDS[0], 1, "first BATCH-MARKER\r\n");
		await injectSession(sessionMgr, SESSION_IDS[1], 2, "second BATCH-MARKER\r\n");
		const { coordLink, sent } = captureLink();

		await handleSearchScrollbackBatch(
			batchFrame("two-sessions", SESSION_IDS.slice(0, 2)),
			"outer-two",
			{ coordLink, sessionMgr, searchOwnerId: "device:tab" },
			FIXED_RUNTIME,
		);

		const result = resultFromOnlyReply(sent);
		expect(result.entries.map(entry => entry.session_id)).toEqual(SESSION_IDS.slice(0, 2));
		for (const entry of result.entries) {
			expect(entry.status).toBe("ok");
			if (entry.status === "ok") {
				expect(entry.result.matches).toHaveLength(1);
				expect(entry.result.grid_epoch.length).toBeGreaterThan(0);
			}
		}
	});

	test("fair per-session caps keep aggregate matches at 256", async () => {
		const sessionMgr = manager();
		const text = Array.from(
			{ length: 180 },
			(_, index) => `BATCH-MARKER ${index}\r\n`,
		).join("");
		await injectSession(sessionMgr, SESSION_IDS[0], 1, text);
		await injectSession(sessionMgr, SESSION_IDS[1], 2, text);
		const { coordLink, sent } = captureLink();

		await handleSearchScrollbackBatch(
			batchFrame("fair", SESSION_IDS.slice(0, 2)),
			"outer-fair",
			{ coordLink, sessionMgr, searchOwnerId: "device:tab" },
			FIXED_RUNTIME,
		);

		const result = resultFromOnlyReply(sent);
		const counts = result.entries.map(entry => entry.status === "ok" ? entry.result.matches.length : 0);
		expect(counts).toEqual([128, 128]);
		expect(counts.reduce((total, count) => total + count, 0)).toBeLessThanOrEqual(256);
	});

	test("one shared work deadline defers sessions not yet started", async () => {
		const sessionMgr = manager();
		const { coordLink, sent } = captureLink();
		let now = 0;
		const calls: SessionId[] = [];
		const observedDeadlines: Array<number | undefined> = [];
		const runtime: _SearchScrollbackBatchRuntime = {
			nowMs: () => now,
			yieldNow: async () => {},
			searchSession: async (frame, requestId, deps, searchRuntime) => {
				calls.push(frame.session_id);
				observedDeadlines.push(searchRuntime?.deadlineAtMs);
				deps.coordLink.send({
					kind: "rpc-ok",
					request_id: requestId,
					data: emptySearchResult("serving-epoch"),
				});
				now = GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS;
			},
		};

		await handleSearchScrollbackBatch(
			batchFrame("deadline", SESSION_IDS.slice(0, 2)),
			"outer-deadline",
			{ coordLink, sessionMgr, searchOwnerId: "device:tab" },
			runtime,
		);

		const result = resultFromOnlyReply(sent);
		expect(calls).toEqual([SESSION_IDS[0]]);
		expect(observedDeadlines).toEqual([GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS]);
		expect(result.entries[0]!.status).toBe("ok");
		expect(result.entries[1]).toEqual({
			status: "error",
			session_id: SESSION_IDS[1],
			error: "deadline",
		});
	});

	test("cancel-before-start tombstones every requested session", async () => {
		const sessionMgr = manager();
		const sessionIds = SESSION_IDS.slice(0, 2);
		cancelSearchScrollbackBatch(cancelFrame("pre-canceled", sessionIds), "device:tab", sessionMgr);
		expect(sessionMgr.terminalSearchCancellations.size).toBe(2);
		const { coordLink, sent } = captureLink();

		await handleSearchScrollbackBatch(
			batchFrame("pre-canceled", sessionIds),
			"outer-canceled",
			{ coordLink, sessionMgr, searchOwnerId: "device:tab" },
			FIXED_RUNTIME,
		);

		const result = resultFromOnlyReply(sent);
		expect(result.entries).toEqual(sessionIds.map(sessionId => ({
			status: "error",
			session_id: sessionId,
			error: "session_closed",
		})));
		expect(sessionMgr.terminalSearchCancellations.size).toBe(0);
	});

	test("a newer viewer batch tombstones all old sessions and stops its loop", async () => {
		const sessionMgr = manager();
		const oldStarted = Promise.withResolvers<void>();
		const releaseOld = Promise.withResolvers<void>();
		const newStarted = Promise.withResolvers<void>();
		const releaseNew = Promise.withResolvers<void>();
		const calls: string[] = [];
		const runtime: _SearchScrollbackBatchRuntime = {
			nowMs: () => 0,
			yieldNow: async () => {},
			searchSession: async (frame, requestId, deps) => {
				calls.push(`${frame.search_id}:${frame.session_id}`);
				if (frame.search_id === "old") {
					oldStarted.resolve();
					await releaseOld.promise;
				} else {
					newStarted.resolve();
					await releaseNew.promise;
				}
				deps.coordLink.send({
					kind: "rpc-ok",
					request_id: requestId,
					data: emptySearchResult(`epoch-${frame.search_id}`),
				});
			},
		};
		const oldCapture = captureLink();
		const oldBatch = handleSearchScrollbackBatch(
			batchFrame("old", SESSION_IDS.slice(0, 2)),
			"outer-old",
			{ coordLink: oldCapture.coordLink, sessionMgr, searchOwnerId: "device:tab" },
			runtime,
		);
		await oldStarted.promise;
		const newCapture = captureLink();
		const newBatch = handleSearchScrollbackBatch(
			batchFrame("new", [SESSION_IDS[2]]),
			"outer-new",
			{ coordLink: newCapture.coordLink, sessionMgr, searchOwnerId: "device:tab" },
			runtime,
		);
		await newStarted.promise;
		expect(sessionMgr.terminalSearchCancellations.size).toBe(2);

		releaseOld.resolve();
		await oldBatch;
		expect(sessionMgr.terminalSearchBatches.get("device:tab")?.searchId).toBe("new");
		expect(calls).not.toContain(`old:${SESSION_IDS[1]}`);
		const oldResult = resultFromOnlyReply(oldCapture.sent);
		expect(oldResult.entries[1]).toEqual({
			status: "error",
			session_id: SESSION_IDS[1],
			error: "deadline",
		});

		releaseNew.resolve();
		await newBatch;
		resultFromOnlyReply(newCapture.sent);
		expect(sessionMgr.terminalSearchBatches.size).toBe(0);
	});

	test("unknown and malformed per-session replies map to closed and internal", async () => {
		const sessionMgr = manager();
		const unknownCapture = captureLink();
		await handleSearchScrollbackBatch(
			batchFrame("unknown", [SESSION_IDS[0]]),
			"outer-unknown",
			{ coordLink: unknownCapture.coordLink, sessionMgr, searchOwnerId: "device:tab" },
			FIXED_RUNTIME,
		);
		expect(resultFromOnlyReply(unknownCapture.sent).entries[0]).toEqual({
			status: "error",
			session_id: SESSION_IDS[0],
			error: "session_closed",
		});

		const malformedCapture = captureLink();
		await handleSearchScrollbackBatch(
			batchFrame("malformed", [SESSION_IDS[1]]),
			"outer-malformed",
			{ coordLink: malformedCapture.coordLink, sessionMgr, searchOwnerId: "device:tab" },
			{
				...FIXED_RUNTIME,
				searchSession: async (_frame, requestId, deps) => {
					deps.coordLink.send({
						kind: "rpc-ok",
						request_id: requestId,
						data: { arbitrary_exception: "must not escape" },
					});
				},
			},
		);
		expect(resultFromOnlyReply(malformedCapture.sent).entries[0]).toEqual({
			status: "error",
			session_id: SESSION_IDS[1],
			error: "internal",
		});
	});

	test("browser command dispatch publishes exactly one batch reply", async () => {
		const sessionMgr = manager();
		const sent: UpstreamFrame[] = [];
		const replied = Promise.withResolvers<void>();
		const coordLink = {
			send: (frame: UpstreamFrame) => {
				sent.push(frame);
				replied.resolve();
				return true;
			},
		} as CoordLink;
		handleBrowserCommand({
			browser_id: "device",
			viewer_id: "device:tab",
			request_id: "outer-dispatch",
			frame: batchFrame("dispatch", SESSION_IDS.slice(0, 2)),
		}, { coordLink, sessionMgr });
		await replied.promise;
		await Promise.resolve();

		const result = resultFromOnlyReply(sent);
		expect(result.entries).toHaveLength(2);
		expect(result.entries.every(entry => entry.status === "error")).toBe(true);
	});
});
