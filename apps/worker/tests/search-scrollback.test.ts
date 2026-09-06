// Bounded scrollback-search tests over terminal cores and one cell fixture.
// Pins cursor seams, structured stop reasons, history floors, Unicode,
// regex progress, cell columns, terminal settling, and boundary validation.

import { describe, expect, test } from "bun:test";
import type { CellData, TerminalCore } from "@wterm/core";
import {
	cellGridEpoch, DEFAULT_COLOR, initCellEmitState, readScrollbackRangeCells,
	scrollbackOrigin, spansText, viewportRowSpans,
} from "@roost/shared/cell";
import {
	TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS,
	countUnicodeCodePoints, type WorkerSearchScrollbackResult,
} from "@roost/shared/terminal-search";
import { ClientControlFrame, asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import {
	cancelSearchScrollback,
	handleSearchScrollback,
	type _SearchScrollbackRuntime,
} from "../src/terminal-search.ts";
import type { CoordLink } from "../src/transport/coord-link-types.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const SESSION_ID = asSessionId("00000000-0000-0000-0000-000000000001");
const CHANNEL_ID = asChannelId(1);
const COLS = 80;
const ROWS = 24;
const LINE_COUNT = 720;
const SEED_TEXT = Array.from({ length: LINE_COUNT }, (_, index) => `FINDLINE-${index}`).join("\r\n") + "\r\n";
const FIXED_RUNTIME: _SearchScrollbackRuntime = {
	nowMs: () => 0,
	yieldNow: async () => {},
};
interface RpcOk {
	kind: "rpc-ok";
	request_id: string;
	data: WorkerSearchScrollbackResult;
}
interface RpcError { kind: "rpc-error"; request_id: string; message: string }
type SearchReply = RpcOk | RpcError;
function freshManager(): SessionManager {
	return new SessionManager({
		workerFp: asWorkerFp("00".repeat(32)),
		sink: new LifecycleTestSink(),
	});
}
async function injectSession(
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
		closeReservation: manager.reserveLifecycleEvent("closed"),
	};
	manager.sessions.set(CHANNEL_ID, record);
	return record;
}
function captureLink(): { coordLink: CoordLink; sent: SearchReply[] } {
	const sent: SearchReply[] = [];
	return {
		coordLink: { send: (frame: SearchReply) => { sent.push(frame); return true; } } as unknown as CoordLink,
		sent,
	};
}

function searchFrame(
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

async function search(
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

function geometry(record: SessionShellRecord): { floor: number; total: number; newest: number } {
	const floor = scrollbackOrigin(record.wtermCore, record.cell_emit);
	const total = floor + record.wtermCore.getScrollbackCount();
	return { floor, total, newest: total + record.wtermCore.getRows() };
}

function rowText(record: SessionShellRecord, absoluteRow: number): string {
	const { floor, total } = geometry(record);
	if (absoluteRow < total) {
		const [row] = readScrollbackRangeCells(record.wtermCore, absoluteRow, absoluteRow + 1, floor);
		expect(row!.index).toBe(absoluteRow);
		return row!.spans.map((span) => span.text).join("").trimEnd();
	}
	return spansText(viewportRowSpans(record.wtermCore, absoluteRow - total, record.wtermCore.getCols())).trimEnd();
}

function installFloor(record: SessionShellRecord, kind: "evicted" | "resize_replay"): number {
	record.cell_emit = { ...record.cell_emit, sbOrigin: 37 };
	const floor = scrollbackOrigin(record.wtermCore, record.cell_emit);
	if (kind === "resize_replay") {
		// The classifier reads only the current replay floor; the remaining pin
		// fields describe rebuild telemetry and do not participate in search.
		record.sb_origin_pin = {
			replay_floor: floor,
		} as NonNullable<SessionShellRecord["sb_origin_pin"]>;
	}
	return floor;
}

describe("bounded search-scrollback", () => {
	test("complete scans include viewport and return newest matches first", async () => {
		const manager = freshManager();
		const record = await injectSession(manager);
		const { floor, total, newest } = geometry(record);
		const middleRow = floor + Math.floor(record.wtermCore.getScrollbackCount() / 2);
		const marker = rowText(record, middleRow);
		const complete = await search(manager, searchFrame(marker));
		expect(complete.stop_reason).toBe("complete");
		expect(complete.truncated).toBe(false);
		expect(complete.next_before_row).toBeUndefined();
		expect([complete.scanned_start_row, complete.scanned_end_row]).toEqual([floor, newest]);
		expect(complete.matches.map((match) => match.row)).toEqual([middleRow]);
		expect(complete.total).toBe(total);

		const empty = await search(manager, searchFrame(""));
		expect(empty.stop_reason).toBe("complete");
		expect(empty.matches).toEqual([]);
		expect([empty.scanned_start_row, empty.scanned_end_row]).toEqual([0, 0]);

		const limited = await search(manager, searchFrame("FINDLINE-", { maxMatches: 3 }));
		expect(limited.stop_reason).toBe("match_limit");
		expect(limited.truncated).toBe(true);
		expect(limited.matches).toHaveLength(3);
		expect(limited.matches[0]!.row).toBeGreaterThanOrEqual(total);
		expect(limited.matches[0]!.row).toBeGreaterThan(limited.matches[1]!.row);
		expect(limited.matches[1]!.row).toBeGreaterThan(limited.matches[2]!.row);
		expect(limited.next_before_row).toBeUndefined();
	});

	test("row-limit pages join at exclusive cursors without duplicate or skipped rows", async () => {
		const manager = freshManager();
		const record = await injectSession(manager);
		const { floor, newest } = geometry(record);
		const pages: WorkerSearchScrollbackResult[] = [];
		let beforeRow: number | undefined;
		for (;;) {
			const page = await search(manager, searchFrame("NO-SUCH-MARKER", {
				beforeRow,
				maxRows: 37,
			}));
			pages.push(page);
			if (page.stop_reason === "complete") break;
			expect(page.stop_reason).toBe("row_limit");
			expect(page.scanned_end_row - page.scanned_start_row).toBe(37);
			expect(page.next_before_row).toBe(page.scanned_start_row);
			expect(page.truncated).toBe(false);
			beforeRow = page.next_before_row;
		}
		expect(pages[0]!.scanned_end_row).toBe(newest);
		expect(pages.at(-1)!.scanned_start_row).toBe(floor);
		for (let index = 1; index < pages.length; index++) {
			expect(pages[index]!.scanned_end_row).toBe(pages[index - 1]!.scanned_start_row);
		}
		const scannedRows = pages.reduce(
			(count, page) => count + page.scanned_end_row - page.scanned_start_row,
			0,
		);
		expect(scannedRows).toBe(newest - floor);
	});

	test("a cap reached exactly at the retained floor is complete, not a false continuation", async () => {
		for (const floorKind of ["evicted", "resize_replay"] as const) {
			const manager = freshManager();
			const record = await injectSession(manager);
			const floor = installFloor(record, floorKind);
			const page = await search(manager, searchFrame(rowText(record, floor), {
				beforeRow: floor + 1,
				maxRows: 1,
				maxMatches: 1,
			}));
			expect(page.stop_reason).toBe("complete");
			expect(page.truncated).toBe(false);
			expect(page.matches).toHaveLength(1);
			expect(page.next_before_row).toBeUndefined();
			expect([page.scanned_start_row, page.scanned_end_row]).toEqual([floor, floor + 1]);
			expect(page.history_floor).toBe(floorKind);
			let timeReads = 0;
			const exhausted = await search(manager, searchFrame("none", { beforeRow: floor }), {
				...FIXED_RUNTIME, nowMs: () => timeReads++ === 0 ? 0 : 8_000,
			});
			expect([exhausted.stop_reason, exhausted.truncated, exhausted.history_floor])
				.toEqual(["complete", false, floorKind]);
			const overflow = await search(manager, searchFrame(".", {
				regex: true, beforeRow: floor + 1, maxRows: 1, maxMatches: 1,
			}));
			expect(overflow.matches).toHaveLength(1);
			expect([overflow.stop_reason, overflow.truncated, overflow.history_floor])
				.toEqual(["match_limit", true, floorKind]);
		}
	});

	test("deadline stops after the last yielded complete row", async () => {
		const manager = freshManager();
		await injectSession(manager);
		let expired = false;
		let yields = 0;
		const page = await search(manager, searchFrame("NO-SUCH-MARKER"), {
			nowMs: () => expired ? Number.MAX_SAFE_INTEGER : 0,
			yieldNow: async () => { yields++; expired = true; },
		});
		expect(page.stop_reason).toBe("deadline");
		expect(page.truncated).toBe(true);
		expect(page.scanned_end_row - page.scanned_start_row).toBe(500);
		expect(page.next_before_row).toBeUndefined();
		expect(yields).toBe(1);
	});

	test("epoch change returns structured incomplete work and never a continuation", async () => {
		const manager = freshManager();
		const record = await injectSession(manager);
		const servingEpoch = cellGridEpoch(record.cell_emit);
		const partial = await search(manager, searchFrame("NO-SUCH-MARKER", { gridEpoch: servingEpoch }), {
			nowMs: () => 0,
			yieldNow: async () => {
				record.cell_emit = {
					...record.cell_emit,
					gridEpochRevision: record.cell_emit.gridEpochRevision + 1,
				};
			},
		});
		expect(partial.stop_reason).toBe("epoch_changed");
		expect(partial.truncated).toBe(false);
		expect(partial.scanned_end_row - partial.scanned_start_row).toBe(500);
		expect(partial.grid_epoch).toBe(servingEpoch);
		expect(partial.next_before_row).toBeUndefined();

		const stale = await search(manager, searchFrame("FINDLINE", { gridEpoch: servingEpoch }));
		expect(stale.stop_reason).toBe("epoch_changed");
		expect(stale.scanned_start_row).toBe(stale.scanned_end_row);
		expect(stale.grid_epoch).toBe(cellGridEpoch(record.cell_emit));
	});

	test("Unicode query and preview bounds count code points without splitting astral text", async () => {
		const manager = freshManager();
		const record = await injectSession(manager, { text: "" });
		const text = "a".repeat(511) + "🐙TARGET";
		// A CellData cluster can contain more code points than grid columns; this
		// isolates the 512-point preview boundary from the core's 256-column cap.
		const cell: CellData = {
			char: 0x61, chars: text, width: 1,
			fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0,
			fgRgb: undefined, bgRgb: undefined,
		};
		record.wtermCore = {
			getCols: () => 1, getRows: () => 1, getCell: () => cell,
			getScrollbackCount: () => 0, getScrollbackDiscardedCount: () => 0,
		} as unknown as TerminalCore;
		const page = await search(manager, searchFrame("🐙TARGET", { maxMatches: 1 }));
		expect(page.matches).toHaveLength(1);
		expect(countUnicodeCodePoints(page.matches[0]!.preview)).toBe(512);
		expect(page.matches[0]!.preview.endsWith("🐙")).toBe(true);
		expect(ClientControlFrame.safeParse(searchFrame("🐙".repeat(256))).success).toBe(true);
	});

	test("linear regex zero-width progress and wide glyph matches use painted columns", async () => {
		const manager = freshManager();
		const record = await injectSession(manager);
		record.wtermCore.writeRaw(new TextEncoder().encode("开始 中文 end\r\n"));
		const wide = await search(manager, searchFrame("中文"));
		expect(wide.matches).toHaveLength(1);
		expect([wide.matches[0]!.col, wide.matches[0]!.len]).toEqual([5, 4]);

		record.wtermCore.writeRaw(new TextEncoder().encode("İTARGET\r\n"));
		const foldedOffset = await search(manager, searchFrame("TARGET"));
		expect(foldedOffset.matches).toHaveLength(1);
		expect(foldedOffset.matches[0]!.col).toBe(1);

		const zeroWidth = await search(manager, searchFrame("^", {
			regex: true,
			beforeRow: wide.matches[0]!.row + 1,
			maxRows: 1,
		}));
		expect(zeroWidth.matches).toHaveLength(1);
		expect([zeroWidth.matches[0]!.col, zeroWidth.matches[0]!.len]).toEqual([0, 0]);
		expect(zeroWidth.stop_reason).toBe("row_limit");

		const { coordLink, sent } = captureLink();
		await handleSearchScrollback(searchFrame("[", { regex: true }), "bad-regex", {
			coordLink,
			sessionMgr: manager,
			searchOwnerId: "browser-a",
		}, FIXED_RUNTIME);
		expect(sent).toHaveLength(1);
		expect(sent[0]!.kind).toBe("rpc-error");
		expect((sent[0] as RpcError).message.startsWith("invalid regex: ")).toBe(true);
	});

	test("search cancellation is isolated by viewer and request identity", async () => {
		const manager = freshManager();
		const record = await injectSession(manager);
		const { promise, resolve } = Promise.withResolvers<void>();
		manager.terminalControlChains.set(CHANNEL_ID, {
			tail: promise, depth: 0,
			running: "terminal_stream", runningSinceMonoMs: 0,
		});
		const firstCapture = captureLink();
		const first = handleSearchScrollback(searchFrame("old", { searchId: "old" }), "old", {
			coordLink: firstCapture.coordLink, sessionMgr: manager, searchOwnerId: "browser-a",
		}, FIXED_RUNTIME);
		await Promise.resolve();
		const otherCapture = captureLink();
		const other = handleSearchScrollback(searchFrame("SETTLED-MARKER", { searchId: "other" }), "other", {
			coordLink: otherCapture.coordLink, sessionMgr: manager, searchOwnerId: "browser-b",
		}, FIXED_RUNTIME);
		await Promise.resolve();
		expect(firstCapture.sent).toHaveLength(0);
		const replacementCapture = captureLink();
		const replacement = handleSearchScrollback(searchFrame("SETTLED-MARKER", { searchId: "replacement" }), "replacement", {
			coordLink: replacementCapture.coordLink, sessionMgr: manager, searchOwnerId: "browser-a",
		}, FIXED_RUNTIME);
		await first;
		expect((firstCapture.sent[0] as RpcError).message).toBe("scrollback search superseded");
		expect(otherCapture.sent).toHaveLength(0);
		cancelSearchScrollback({
			kind: "cancel-scrollback-search",
			request_id: "cancel-other",
			session_id: SESSION_ID,
			search_request_id: "other",
		}, "browser-b", manager);
		await other;
		expect((otherCapture.sent[0] as RpcError).message).toBe("scrollback search superseded");
		record.wtermCore.writeRaw(new TextEncoder().encode("SETTLED-MARKER\r\n"));
		resolve();
		await replacement;
		expect((replacementCapture.sent[0] as RpcOk).data.matches).toHaveLength(1);
	});
});
