// Bounded scrollback-search matching tests: code-point preview bounds over
// astral text, wide-glyph and case-folded column mapping, zero-width regex
// progress and invalid-regex rejection. Paging seams live in
// search-scrollback.test.ts; the SessionManager fixture is
// search-scrollback-test-harness.ts.

import { describe, expect, test } from "bun:test";
import type { CellData, TerminalCore } from "@wterm/core";
import { DEFAULT_COLOR } from "@roost/shared/cell";
import { countUnicodeCodePoints } from "@roost/shared/terminal-search";
import { ClientControlFrame } from "@roost/shared/wire";
import { handleSearchScrollback } from "../src/terminal-search.ts";
import {
	FIXED_RUNTIME, captureLink, freshManager, injectSession, search, searchFrame,
	type RpcError,
} from "./search-scrollback-test-harness.ts";

describe("bounded search-scrollback", () => {
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
});
