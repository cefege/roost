// Bounded terminal-find page-chain and epoch-fence tests.
// They pin exclusive cursor progression, terminal partial states, cancellation,
// reading-order publication, and the single retry after grid renumbering.

import { afterAll, beforeAll, beforeEach, describe, expect, mock, test } from "bun:test";
import { ScrollbackHistoryFloor, SearchStopReason } from "@roost/shared/proto/coordinator_pb";
import { TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_PAGES, TERMINAL_SEARCH_MAX_ROWS } from "@roost/shared/terminal-search";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { FindHit } from "../src/lib/cellRow.ts";
import type { ScrollbackBackfill } from "../src/lib/scrollbackBackfill.ts";
interface SearchRequest {
  sessionId: string; searchId: string; gridEpoch: string; query: string;
  caseSensitive: boolean; regex: boolean; beforeRow?: bigint;
  maxRows: number; maxMatches: number;
}
interface SearchResponse {
  matches: Array<{ row: bigint; col: number; len: number; preview: string }>;
  truncated: boolean; scrollbackTotal: bigint; cols: number; gridEpoch: string;
  scannedStartRow: bigint; scannedEndRow: bigint;
  historyFloor: ScrollbackHistoryFloor; nextBeforeRow?: bigint;
  stopReason: SearchStopReason;
}
interface SearchCallOptions { signal?: AbortSignal }
const requests: SearchRequest[] = [];
const signals: AbortSignal[] = [];
const cancellationRequests: Array<{ sessionId: string; searchId: string }> = [];
let rpcImpl: (request: SearchRequest, options?: SearchCallOptions) => Promise<SearchResponse>;
mock.module("../src/connect.ts", () => ({
  coordClient: {
    sessionsSearchScrollback(request: SearchRequest, options?: SearchCallOptions) {
      requests.push(request);
      if (options?.signal) signals.push(options.signal);
      return rpcImpl(request, options);
    },
    async sessionsCancelScrollbackSearch(request: { sessionId: string; searchId: string }) {
      cancellationRequests.push(request);
      return {};
    },
  },
}));

const { createTerminalFind } = await import("../src/lib/terminalFindController.ts");

const EPOCH_A = "grid-a:0";
const EPOCH_B = "grid-b:0";

interface ReplyOptions { stop?: SearchStopReason; start?: number; end?: number; next?: number }

function reply(rows: number[], gridEpoch: string, options: ReplyOptions = {}): SearchResponse {
  const stopReason = options.stop ?? SearchStopReason.COMPLETE;
  return {
    matches: rows.map((row) => ({ row: BigInt(row), col: 3, len: 4, preview: `line ${row}` })),
    truncated: stopReason === SearchStopReason.MATCH_LIMIT || stopReason === SearchStopReason.DEADLINE,
    scrollbackTotal: 2000n,
    cols: 80,
    gridEpoch,
    scannedStartRow: BigInt(options.start ?? 0),
    scannedEndRow: BigInt(options.end ?? 2000),
    historyFloor: ScrollbackHistoryFloor.UNSPECIFIED,
    ...(options.next === undefined ? {} : { nextBeforeRow: BigInt(options.next) }),
    stopReason,
  };
}

interface Published { rows: number[]; active: { row: number; col: number } | null }

function harness() {
  const anchor = { sbBase: 500, cols: 80, total: 2000, gridEpoch: EPOCH_A };
  const jumps: number[] = [];
  const pulled: number[] = [];
  const published: Published[] = [];
  const renderer = {
    backfillAnchor: () => ({ ...anchor }),
    setFindHighlights(
      hits: ReadonlyMap<number, FindHit[]>,
      active: { row: number; col: number } | null,
    ) {
      published.push({ rows: Array.from(hits.keys()), active });
    },
    scrollToScrollbackRow(absIndex: number) { jumps.push(absIndex); },
  } as unknown as CellGridRenderer;
  const backfill = {
    async ensureRowPainted(absIndex: number) {
      pulled.push(absIndex);
      return true;
    },
  } as unknown as ScrollbackBackfill;
  const find = createTerminalFind({
    sessionId: "session-1", renderer: () => renderer, backfill: () => backfill,
  });
  return {
    anchor, jumps, pulled, published, find,
    last(): Published { return published[published.length - 1] ?? { rows: [], active: null }; },
  };
}

// Capture debounce timers so tests never sleep through FIND_DEBOUNCE_MS.
const pendingTimers = new Map<number, () => void>();
let nextTimerId = 1;
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;

beforeAll(() => {
  Object.defineProperty(globalThis, "setTimeout", {
    configurable: true,
    value: (fn: () => void) => {
      const id = nextTimerId++;
      pendingTimers.set(id, fn);
      return id;
    },
  });
  Object.defineProperty(globalThis, "clearTimeout", {
    configurable: true,
    value: (id?: number) => { if (id !== undefined) pendingTimers.delete(id); },
  });
});
afterAll(() => {
  Object.defineProperty(globalThis, "setTimeout", { configurable: true, value: realSetTimeout });
  Object.defineProperty(globalThis, "clearTimeout", { configurable: true, value: realClearTimeout });
});

async function settle(): Promise<void> {
  for (let idx = 0; idx < 30; idx++) await Promise.resolve();
}

async function fireDebounce(): Promise<void> {
  const due = Array.from(pendingTimers.values());
  pendingTimers.clear();
  for (const fn of due) fn();
  await settle();
}

beforeEach(() => {
  requests.length = 0;
  signals.length = 0;
  cancellationRequests.length = 0;
  pendingTimers.clear();
  rpcImpl = async () => reply([], EPOCH_A);
});

describe("terminal find paging and epoch fence", () => {
  test("F1 — a same-epoch hit pulls its row in and reveals it", async () => {
    const h = harness();
    rpcImpl = async () => reply([120], EPOCH_A);
    h.find.setQuery("boom");
    await fireDebounce();
    expect(requests).toHaveLength(1);
    expect(requests[0]!.gridEpoch).toBe(EPOCH_A);
    expect(h.find.matches().map((match) => [match.row, match.epoch])).toEqual([[120, EPOCH_A]]);
    expect(h.find.index()).toBe(1);
    expect(h.pulled).toEqual([120]);
    expect(h.jumps).toEqual([120]);
    expect(h.last()).toEqual({ rows: [120], active: { row: 120, col: 3 } });
  });

  test("F2 — a retired-epoch set is discarded and re-searched before reveal", async () => {
    const h = harness();
    rpcImpl = async () => reply([1200], EPOCH_A);
    h.find.setQuery("boom");
    await fireDebounce();
    h.jumps.length = 0;
    h.anchor.gridEpoch = EPOCH_B;
    h.anchor.sbBase = 0;
    h.anchor.total = 1500;
    rpcImpl = async () => reply([80], EPOCH_B);
    h.find.step(1);
    await settle();
    expect(requests.map((request) => request.gridEpoch)).toEqual([EPOCH_A, EPOCH_B]);
    expect(h.jumps).not.toContain(1200);
    expect(h.find.matches().map((match) => [match.row, match.epoch])).toEqual([[80, EPOCH_B]]);
    expect(h.jumps).toEqual([80]);
  });

  test("F2b — a stale set stays discarded when the retry finds nothing", async () => {
    const h = harness();
    rpcImpl = async () => reply([1200], EPOCH_A);
    h.find.setQuery("boom");
    await fireDebounce();
    h.jumps.length = 0;
    h.anchor.gridEpoch = EPOCH_B;
    rpcImpl = async () => reply([], EPOCH_B);
    h.find.step(1);
    await settle();
    expect(h.jumps).toEqual([]);
    expect(h.find.matches()).toEqual([]);
    expect(h.find.index()).toBe(0);
    expect(h.last()).toEqual({ rows: [], active: null });
    expect(h.find.failed()).toBe(false);
  });

  test("F3 — a refused moved epoch re-asks once against the displayed grid", async () => {
    const h = harness();
    rpcImpl = async (request) => {
      if (request.gridEpoch === EPOCH_A) {
        h.anchor.gridEpoch = EPOCH_B;
        throw new Error("grid epoch changed");
      }
      return reply([700], EPOCH_B);
    };
    h.find.setQuery("boom");
    await fireDebounce();
    expect(requests.map((request) => request.gridEpoch)).toEqual([EPOCH_A, EPOCH_B]);
    expect(h.find.failed()).toBe(false);
    expect(h.find.matches().map((match) => match.row)).toEqual([700]);
    expect(h.jumps).toEqual([700]);
  });

  test("F4 — a repeatedly moving epoch spends only one retry", async () => {
    const h = harness();
    let flip = 0;
    rpcImpl = async () => {
      h.anchor.gridEpoch = `grid-${++flip}:0`;
      throw new Error("grid epoch changed");
    };
    h.find.setQuery("boom");
    await fireDebounce();
    expect(requests).toHaveLength(2);
    expect(h.find.failed()).toBe(true);
    expect(h.find.matches()).toEqual([]);
  });

  test("F5 — an ordinary RPC or regex failure does not retry", async () => {
    const h = harness();
    rpcImpl = async () => { throw new Error("invalid regex"); };
    h.find.setQuery("*");
    await fireDebounce();
    expect(requests).toHaveLength(1);
    expect(h.find.failed()).toBe(true);
    expect(h.find.matches()).toEqual([]);
    expect(h.jumps).toEqual([]);
  });

  test("chains sparse pages sequentially with exclusive, non-overlapping cursors", async () => {
    const h = harness();
    let inFlight = 0;
    let maxInFlight = 0;
    rpcImpl = async (request) => {
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
    };
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
    rpcImpl = async () => reply(
      Array.from({ length: TERMINAL_SEARCH_MAX_MATCHES }, (_, idx) => 1999 - idx),
      EPOCH_A,
      { stop: SearchStopReason.MATCH_LIMIT },
    );
    capped.find.setQuery("many");
    await fireDebounce();
    expect(capped.find.matches()).toHaveLength(TERMINAL_SEARCH_MAX_MATCHES);
    expect(capped.find.truncated()).toBe(true);
    expect(capped.find.failed()).toBe(false);

    const timed = harness();
    rpcImpl = async () => reply([700], EPOCH_A, { stop: SearchStopReason.DEADLINE });
    timed.find.setQuery("slow");
    await fireDebounce();
    expect(timed.find.matches().map((match) => match.row)).toEqual([700]);
    expect(timed.find.truncated()).toBe(true);
    expect(timed.find.failed()).toBe(true);

    const emptyTimed = harness();
    rpcImpl = async () => reply([], EPOCH_A, { stop: SearchStopReason.DEADLINE, start: 0, end: 0 });
    emptyTimed.find.setQuery("too slow");
    await fireDebounce();
    expect(emptyTimed.find.matches()).toEqual([]);
    expect(emptyTimed.find.truncated()).toBe(true);
    expect(emptyTimed.find.failed()).toBe(true);
  });

  test("later-page epoch change discards the chain and retries from newest", async () => {
    const h = harness();
    let call = 0;
    rpcImpl = async () => {
      call++;
      if (call === 1) {
        return reply([1500], EPOCH_A, {
          stop: SearchStopReason.ROW_LIMIT, start: 1000, end: 2000, next: 1000,
        });
      }
      if (call === 2) {
        h.anchor.gridEpoch = EPOCH_B;
        return reply([900], EPOCH_A, { stop: SearchStopReason.EPOCH_CHANGED });
      }
      return reply([80], EPOCH_B);
    };
    h.find.setQuery("moving");
    await fireDebounce();
    expect(requests.map((request) => request.gridEpoch)).toEqual([EPOCH_A, EPOCH_A, EPOCH_B]);
    expect(requests.map((request) => request.beforeRow)).toEqual([undefined, 1000n, undefined]);
    expect(h.find.matches().map((match) => [match.row, match.epoch])).toEqual([[80, EPOCH_B]]);
    expect(h.find.failed()).toBe(false);
  });

  test("malformed ranges and nonprogressing cursors fail without another page", async () => {
    const stuck = harness();
    rpcImpl = async () => reply([], EPOCH_A, {
      stop: SearchStopReason.ROW_LIMIT, start: 1000, end: 2000, next: 2000,
    });
    stuck.find.setQuery("stuck");
    await fireDebounce();
    expect(requests).toHaveLength(1);
    expect(stuck.find.failed()).toBe(true);
    stuck.find.dispose();
    requests.length = 0;
    const overlap = harness();
    let call = 0;
    rpcImpl = async () => ++call === 1
      ? reply([1500], EPOCH_A, {
          stop: SearchStopReason.ROW_LIMIT, start: 1000, end: 2000, next: 1000,
        })
      : reply([500], EPOCH_A, { start: 0, end: 1500 });
    overlap.find.setQuery("overlap");
    await fireDebounce();
    expect(requests).toHaveLength(2);
    expect(overlap.find.matches().map((match) => match.row)).toEqual([1500]);
    expect(overlap.find.failed()).toBe(true);
    requests.length = 0;
    const bounded = harness();
    rpcImpl = async (request) => {
      const end = Number(request.beforeRow ?? 2000n);
      return reply([], EPOCH_A, { stop: SearchStopReason.ROW_LIMIT, start: end - 1, end, next: end - 1 });
    };
    bounded.find.setQuery("bounded chain");
    await fireDebounce();
    expect(requests).toHaveLength(TERMINAL_SEARCH_MAX_PAGES);
    expect(bounded.find.failed()).toBe(true);
  });

  test("a new query aborts the old chain; its stale page cannot publish or continue", async () => {
    const h = harness();
    const old = Promise.withResolvers<SearchResponse>();
    rpcImpl = (request) => request.query === "old"
      ? old.promise
      : Promise.resolve(reply([80], EPOCH_A));
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
