// Terminal-find page-chain tests: exclusive cursor progression across pages,
// capped partial states, paging back past the match cap, malformed-range and
// nonprogressing-cursor failures, and query cutover with cancellation.
// The epoch fence lives in terminalFindController.test.ts; the mocked RPC and
// debounce clock are terminalFindController-test-harness.ts.

import { describe, expect, test } from "bun:test";
import { SearchStopReason } from "@roost/shared/proto/coordinator_pb";
import { TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_PAGES, TERMINAL_SEARCH_MAX_ROWS } from "@roost/shared/terminal-search";
import {
  EPOCH_A, cancellationRequests, fireDebounce, createFindHarness, installTerminalFindTestLifecycle,
  reply, requests, setSearchRpc, settle, signals, type SearchResponse,
} from "./terminalFindController-test-harness.ts";

const { createTerminalFind } = await import("../src/lib/terminalFindController.ts");

function harness() {
  return createFindHarness(createTerminalFind);
}

installTerminalFindTestLifecycle();

describe("terminal find paging and epoch fence", () => {
  test("chains sparse pages sequentially with exclusive, non-overlapping cursors", async () => {
    const h = harness();
    let inFlight = 0;
    let maxInFlight = 0;
    setSearchRpc(async (request) => {
      inFlight++;
      maxInFlight = Math.max(maxInFlight, inFlight);
      await Promise.resolve();
      const response = request.beforeRow === undefined
        ? reply([1900], EPOCH_A, {
            stop: SearchStopReason.ROW_LIMIT, start: 1200, end: 2000, next: 1200,
          })
        : request.beforeRow === 1200n
          ? reply([], EPOCH_A, {
              stop: SearchStopReason.ROW_LIMIT, start: 400, end: 1200, next: 400,
            })
          : reply([50], EPOCH_A, { start: 0, end: 400 });
      inFlight--;
      return response;
    });
    h.find.setQuery("sparse");
    await fireDebounce();
    expect(maxInFlight).toBe(1);
    expect(Object.hasOwn(requests[0]!, "beforeRow")).toBe(false);
    expect(requests.map((request) => request.beforeRow)).toEqual([undefined, 1200n, 400n]);
    expect(requests.map((request) => request.maxRows))
      .toEqual([TERMINAL_SEARCH_MAX_ROWS, TERMINAL_SEARCH_MAX_ROWS, TERMINAL_SEARCH_MAX_ROWS]);
    expect(requests.map((request) => request.maxMatches))
      .toEqual([TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_MATCHES - 1, TERMINAL_SEARCH_MAX_MATCHES - 1]);
    expect(h.find.matches().map((match) => match.row)).toEqual([50, 1900]);
    expect(h.find.index()).toBe(2);
    expect(h.find.truncated()).toBe(false);
    expect(h.find.failed()).toBe(false);
  });

  test("match-limit and deadline publish explicit capped partial states", async () => {
    const capped = harness();
    setSearchRpc(async () => reply(
      Array.from({ length: TERMINAL_SEARCH_MAX_MATCHES }, (_, idx) => 1999 - idx),
      EPOCH_A,
      { stop: SearchStopReason.MATCH_LIMIT },
    ));
    capped.find.setQuery("many");
    await fireDebounce();
    expect(capped.find.matches()).toHaveLength(TERMINAL_SEARCH_MAX_MATCHES);
    expect(capped.find.truncated()).toBe(true);
    expect(capped.find.failed()).toBe(false);

    const timed = harness();
    setSearchRpc(async () => reply([700], EPOCH_A, { stop: SearchStopReason.DEADLINE }));
    timed.find.setQuery("slow");
    await fireDebounce();
    expect(timed.find.matches().map((match) => match.row)).toEqual([700]);
    expect(timed.find.truncated()).toBe(true);
    expect(timed.find.failed()).toBe(true);

    const emptyTimed = harness();
    setSearchRpc(async () => reply([], EPOCH_A, { stop: SearchStopReason.DEADLINE, start: 0, end: 0 }));
    emptyTimed.find.setQuery("too slow");
    await fireDebounce();
    expect(emptyTimed.find.matches()).toEqual([]);
    expect(emptyTimed.find.truncated()).toBe(true);
    expect(emptyTimed.find.failed()).toBe(true);
  });

  test("stepping back past the oldest match pages beyond the match cap", async () => {
    const h = harness();
    const fullPage = (newestRow: number): number[] => Array.from(
      { length: TERMINAL_SEARCH_MAX_MATCHES },
      (_, idx) => newestRow - idx,
    );
    setSearchRpc(async (request) => {
      if (request.beforeRow === undefined) {
        return reply(fullPage(1999), EPOCH_A, {
          stop: SearchStopReason.MATCH_LIMIT, start: 1744, end: 2000, next: 1744,
        });
      }
      if (request.beforeRow === 1744n) {
        return reply(fullPage(1743), EPOCH_A, {
          stop: SearchStopReason.MATCH_LIMIT, start: 1488, end: 1744, next: 1488,
        });
      }
      return reply([1000], EPOCH_A, { start: 0, end: 1488 });
    });
    const stepBackOntoOldest = async (): Promise<void> => {
      while (h.find.index() > 1) h.find.step(-1);
      await settle();
      h.find.step(-1);
      await settle();
    };
    h.find.setQuery("many");
    await fireDebounce();
    expect(h.find.matches()).toHaveLength(TERMINAL_SEARCH_MAX_MATCHES);
    expect(h.find.truncated()).toBe(true);
    expect(h.find.failed()).toBe(false);

    await stepBackOntoOldest();
    expect(requests).toHaveLength(2);
    expect(requests[1]!.beforeRow).toBe(1744n);
    expect(requests[1]!.maxMatches).toBe(TERMINAL_SEARCH_MAX_MATCHES);
    const twoPages = h.find.matches().map((match) => match.row);
    expect(twoPages).toHaveLength(TERMINAL_SEARCH_MAX_MATCHES * 2);
    expect(new Set(twoPages).size).toBe(twoPages.length);
    expect(Math.max(...twoPages.slice(0, TERMINAL_SEARCH_MAX_MATCHES))).toBeLessThan(1744);
    // The newest row of the page just paged in becomes active and is revealed.
    expect(h.find.matches()[h.find.index() - 1]!.row).toBe(1743);
    expect(h.jumps.at(-1)).toBe(1743);
    expect(h.find.truncated()).toBe(true);

    await stepBackOntoOldest();
    expect(requests).toHaveLength(3);
    expect(requests[2]!.beforeRow).toBe(1488n);
    const allRows = h.find.matches().map((match) => match.row);
    expect(allRows).toHaveLength(TERMINAL_SEARCH_MAX_MATCHES * 2 + 1);
    expect(new Set(allRows).size).toBe(allRows.length);
    expect([...allRows].sort((left, right) => left - right)).toEqual(allRows);
    expect(h.find.truncated()).toBe(false);
    expect(h.find.failed()).toBe(false);

    // The last page completed, so stepping off the oldest match wraps instead
    // of asking for a page that cannot exist.
    await stepBackOntoOldest();
    expect(requests).toHaveLength(3);
    expect(h.find.index()).toBe(allRows.length);
  });

  test("malformed ranges and nonprogressing cursors fail without another page", async () => {
    const stuck = harness();
    setSearchRpc(async () => reply([], EPOCH_A, {
      stop: SearchStopReason.ROW_LIMIT, start: 1000, end: 2000, next: 2000,
    }));
    stuck.find.setQuery("stuck");
    await fireDebounce();
    expect(requests).toHaveLength(1);
    expect(stuck.find.failed()).toBe(true);
    stuck.find.dispose();
    requests.length = 0;
    const overlap = harness();
    let call = 0;
    setSearchRpc(async () => ++call === 1
      ? reply([1500], EPOCH_A, {
          stop: SearchStopReason.ROW_LIMIT, start: 1000, end: 2000, next: 1000,
        })
      : reply([500], EPOCH_A, { start: 0, end: 1500 }));
    overlap.find.setQuery("overlap");
    await fireDebounce();
    expect(requests).toHaveLength(2);
    expect(overlap.find.matches().map((match) => match.row)).toEqual([1500]);
    expect(overlap.find.failed()).toBe(true);
    requests.length = 0;
    const bounded = harness();
    setSearchRpc(async (request) => {
      const end = Number(request.beforeRow ?? 2000n);
      return reply([], EPOCH_A, { stop: SearchStopReason.ROW_LIMIT, start: end - 1, end, next: end - 1 });
    });
    bounded.find.setQuery("bounded chain");
    await fireDebounce();
    expect(requests).toHaveLength(TERMINAL_SEARCH_MAX_PAGES);
    expect(bounded.find.failed()).toBe(true);
  });

  test("a new query aborts the old chain; its stale page cannot publish or continue", async () => {
    const h = harness();
    const old = Promise.withResolvers<SearchResponse>();
    setSearchRpc((request) => request.query === "old"
      ? old.promise
      : Promise.resolve(reply([80], EPOCH_A)));
    h.find.setQuery("old");
    await fireDebounce();
    h.find.setQuery("new");
    expect(signals[0]!.aborted).toBe(true);
    expect(cancellationRequests).toEqual([{
      sessionId: "session-1",
      searchId: requests[0]!.searchId,
    }]);
    await fireDebounce();
    expect(h.find.matches().map((match) => match.row)).toEqual([80]);
    old.resolve(reply([1500], EPOCH_A, {
      stop: SearchStopReason.ROW_LIMIT, start: 1000, end: 2000, next: 1000,
    }));
    await settle();
    expect(requests.map((request) => request.query)).toEqual(["old", "new"]);
    expect(h.find.matches().map((match) => match.row)).toEqual([80]);
  });

  test("regex and case flags survive the paged request cutover", async () => {
    const h = harness();
    h.find.setQuery("a.*b");
    h.find.toggleRegex();
    h.find.toggleCaseSensitive();
    await fireDebounce();
    expect(requests).toHaveLength(1);
    expect(requests[0]).toMatchObject({
      sessionId: "session-1",
      gridEpoch: EPOCH_A,
      query: "a.*b",
      caseSensitive: true,
      regex: true,
      maxRows: TERMINAL_SEARCH_MAX_ROWS,
      maxMatches: TERMINAL_SEARCH_MAX_MATCHES,
    });
    expect(requests[0]!.searchId).toHaveLength(36);
  });
});
