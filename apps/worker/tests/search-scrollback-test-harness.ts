// Shared fixture for the bounded scrollback-search suites: a SessionManager
// holding one injected shell record over a real wterm core, plus the frame,
// runtime and coord-link capture helpers those suites drive
// handleSearchScrollback with. Used by search-scrollback.test.ts,
// search-scrollback-matching.test.ts and search-scrollback-cancellation.test.ts;
// depends on ../src/session-manager.ts and ../src/terminal-search.ts.

import { expect } from "bun:test";
import { initCellEmitState } from "@roost/shared/cell";
import {
	TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS,
	type WorkerSearchScrollbackResult,
} from "@roost/shared/terminal-search";
import { ClientControlFrame, asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { handleSearchScrollback, type _SearchScrollbackRuntime } from "../src/terminal-search.ts";
import type { CoordLink } from "../src/transport/coord-link-types.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

export const SESSION_ID = asSessionId("00000000-0000-0000-0000-000000000001");
export const CHANNEL_ID = asChannelId(1);
const COLS = 80;
const ROWS = 24;
const LINE_COUNT = 720;
const SEED_TEXT = Array.from({ length: LINE_COUNT }, (_, index) => `FINDLINE-${index}`).join("\r\n") + "\r\n";
export const FIXED_RUNTIME: _SearchScrollbackRuntime = {
	nowMs: () => 0,
	yieldNow: async () => {},
};
export interface RpcOk {
	kind: "rpc-ok";
	request_id: string;
	data: WorkerSearchScrollbackResult;
}
export interface RpcError { kind: "rpc-error"; request_id: string; message: string }
type SearchReply = RpcOk | RpcError;
export function freshManager(): SessionManager {
	return new SessionManager({
		workerFp: asWorkerFp("00".repeat(32)),
		sink: new SessionEventTestSink(),
	});
}
export async function injectSession(
	manager: SessionManager,
	options: { cols?: number; rows?: number; text?: string } = {},
): Promise<SessionShellRecord> {
	const cols = options.cols ?? COLS;
	const rows = options.rows ?? ROWS;
	const text = options.text ?? SEED_TEXT;
	const bytes = new TextEncoder().encode(text);
	const wtermCore = await createWtermCore(cols, rows);
	wtermCore.writeRaw(bytes);
	const record: SessionShellRecord = {
		sessionId: SESSION_ID,
		channelId: CHANNEL_ID,
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
		session_trace_id: "sbfind00",
		cell_emit: initCellEmitState("test-grid", "00000000-0000-4000-8000-000000000001"),
		lastPtyOutMs: 0,
		sb_origin_pin: null,
		spawnedAtMs: Date.now(),
		closeReservation: manager.reserveSessionEvent("closed"),
	};
	manager.sessions.set(CHANNEL_ID, record);
	return record;
}
export function captureLink(): { coordLink: CoordLink; sent: SearchReply[] } {
	const sent: SearchReply[] = [];
	return {
		coordLink: { send: (frame: SearchReply) => { sent.push(frame); return true; } } as unknown as CoordLink,
		sent,
	};
}

export function searchFrame(
	query: string,
	options: {
		beforeRow?: number;
		maxRows?: number;
		maxMatches?: number;
		gridEpoch?: string;
		caseSensitive?: boolean;
		regex?: boolean;
		searchId?: string;
	} = {},
): Extract<ClientControlFrame, { kind: "search-scrollback" }> {
	return {
		kind: "search-scrollback",
		request_id: "inner-request",
		session_id: SESSION_ID,
		search_id: options.searchId ?? "search-id",
		grid_epoch: options.gridEpoch ?? "",
		query,
		case_sensitive: options.caseSensitive ?? false,
		regex: options.regex ?? false,
		...(options.beforeRow === undefined ? {} : { before_row: options.beforeRow }),
		max_rows: options.maxRows ?? TERMINAL_SEARCH_MAX_ROWS,
		max_matches: options.maxMatches ?? TERMINAL_SEARCH_MAX_MATCHES,
	};
}

export async function search(
	manager: SessionManager,
	frame: Extract<ClientControlFrame, { kind: "search-scrollback" }>,
	runtime: _SearchScrollbackRuntime = FIXED_RUNTIME,
): Promise<WorkerSearchScrollbackResult> {
	const { coordLink, sent } = captureLink();
	await handleSearchScrollback(frame, "outer-request", {
		coordLink, sessionMgr: manager, searchOwnerId: "browser-a",
	}, runtime);
	expect(sent).toHaveLength(1);
	expect(sent[0]!.kind).toBe("rpc-ok");
	return (sent[0] as RpcOk).data;
}
