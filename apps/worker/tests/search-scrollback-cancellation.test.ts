// Bounded scrollback-search cancellation tests: a second search from the same
// viewer supersedes the first, an explicit cancel frame only reaches its own
// viewer's request, and the surviving search still answers once the terminal
// control chain settles. Paging seams live in search-scrollback.test.ts; the
// SessionManager fixture is search-scrollback-test-harness.ts.

import { describe, expect, test } from "bun:test";
import { cancelSearchScrollback, handleSearchScrollback } from "../src/terminal-search.ts";
import {
	CHANNEL_ID, FIXED_RUNTIME, SESSION_ID, captureLink, freshManager, injectSession, searchFrame,
	type RpcError, type RpcOk,
} from "./search-scrollback-test-harness.ts";

describe("bounded search-scrollback", () => {
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
