// Bounded scrollback-search paging tests: cursor seams between pages,
// structured stop reasons, retained-history floors and epoch invalidation.
// Unicode/regex matching lives in search-scrollback-matching.test.ts and
// supersede/cancel isolation in search-scrollback-cancellation.test.ts; the
// SessionManager fixture is search-scrollback-test-harness.ts.

import { describe, expect, test } from "bun:test";
import {
	cellGridEpoch, readScrollbackRangeCells, scrollbackOrigin, spansText, viewportRowSpans,
} from "@roost/shared/cell";
import {
	TERMINAL_SEARCH_MAX_MATCHES, type WorkerSearchScrollbackResult,
} from "@roost/shared/terminal-search";
import type { SessionShellRecord } from "../src/session-record.ts";
import {
	FIXED_RUNTIME, freshManager, injectSession, search, searchFrame,
} from "./search-scrollback-test-harness.ts";

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
		expect(complete.scrollback_total).toBe(total);

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
		expect(limited.next_before_row).toBe(limited.scanned_start_row);
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
			// The cap landed on the floor row itself: no older row can be served,
			// so the page must not hand back a cursor that scans nothing.
			expect(overflow.next_before_row).toBeUndefined();
		}
	});

	test("match-limit pages reach matches older than one page can hold", async () => {
		const manager = freshManager();
		const record = await injectSession(manager);
		const { floor } = geometry(record);
		const rows: number[] = [];
		let beforeRow: number | undefined;
		let pages = 0;
		for (;;) {
			const page = await search(manager, searchFrame("FINDLINE-", { beforeRow }));
			pages++;
			expect(pages).toBeLessThan(10);
			for (const match of page.matches) rows.push(match.row);
			if (page.stop_reason !== "match_limit") {
				expect(page.stop_reason).toBe("complete");
				expect(page.next_before_row).toBeUndefined();
				expect(page.scanned_start_row).toBe(floor);
				break;
			}
			expect(page.matches).toHaveLength(TERMINAL_SEARCH_MAX_MATCHES);
			expect(page.next_before_row).toBe(page.scanned_start_row);
			beforeRow = page.next_before_row;
		}
		expect(pages).toBeGreaterThan(2);
		expect(rows.length).toBeGreaterThan(TERMINAL_SEARCH_MAX_MATCHES);
		expect(new Set(rows).size).toBe(rows.length);
		for (let index = 1; index < rows.length; index++) {
			expect(rows[index]!).toBeLessThan(rows[index - 1]!);
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
});
