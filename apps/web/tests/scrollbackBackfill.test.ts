// Scrollback demand-paging tests exercise request fencing without a browser DOM.
// The renderer stand-in, the response builder and the drain live in
// ./scrollbackBackfill-test-harness.ts; page geometry is pinned directly in
// scrollbackBackfill.bounds.test.ts, and DOM insertion plus placeholder
// ownership remain covered by the renderer DOM tests.

import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import { ScrollbackHistoryFloor } from "@roost/shared/proto/coordinator_pb";
import {
  createBackfillHarness, flushWork, GRID_EPOCH, installScrollbackFrameLifecycle, INTERIOR_PAINTED,
  response, type BackfillHarnessOptions, type ScrollRequest, type ScrollResponse,
} from "./scrollbackBackfill-test-harness.ts";

const rpcCalls: ScrollRequest[] = [];
let rpcImpl: (request: ScrollRequest) => Promise<ScrollResponse>;

mock.module("../src/connect.ts", () => ({
  coordClient: {
    sessionsGetScrollbackCells(request: ScrollRequest) { rpcCalls.push(request); return rpcImpl(request); },
  },
}));

const diagEvents: Array<Record<string, unknown>> = [];
// Read before mock.module replaces the specifier, so the override can spread
// the real module's other exports.
const realDiag = await import("@roost/shared/diag");
mock.module("@roost/shared/diag", () => ({
  ...realDiag,
  diag(evt: string, kv: Record<string, unknown>) { diagEvents.push({ evt, ...kv }); },
}));

// Imported after the mocks above: a hoisted static import would bind the
// controller to the real RPC client and the real diag sink.
const {
  BACKFILL_RETRY_MS,
  createScrollbackBackfill,
  scrollbackHistoryFloor,
} = await import("../src/lib/scrollbackBackfill.ts");

function harness(options: BackfillHarnessOptions = {}) {
  return createBackfillHarness(createScrollbackBackfill, options);
}

installScrollbackFrameLifecycle();

beforeEach(() => {
  rpcCalls.length = 0;
  diagEvents.length = 0;
  rpcImpl = async (request) => response(Number(request.endRow) - request.maxRows, Number(request.endRow), 760);
});
afterEach(() => vi.useRealTimers());


describe("ScrollbackBackfill arbitrary gap paging", () => {
  test("fills a bounded short tail gap from its reader focus", async () => {
    const h = harness({ total: 760, painted: Array.from({ length: 250 }, (_, index) => index + 500), focus: 755 });
    rpcImpl = async (request) => response(750, Number(request.endRow), 760);

    h.controller.onUserScroll();
    await flushWork();
    expect(h.insertions[0]!.map((row) => row.index)).toEqual(
      Array.from({ length: 10 }, (_, index) => index + 750),
    );
    expect(h.painted.has(755)).toBe(true);
    h.controller.dispose();
  });

  test("pages head and interior gaps without inferring coverage from sbBase", async () => {
    const head = harness({ total: 300, focus: 50 });
    rpcImpl = async (request) => response(0, Number(request.endRow), 300);
    head.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls[0]!.endRow).toBe(250n);
    expect(rpcCalls[0]!.maxRows).toBe(250);

    rpcCalls.length = 0;
    const interior = harness({ total: 300, painted: INTERIOR_PAINTED, focus: 150 });
    rpcImpl = async (request) => response(100, Number(request.endRow), 300);
    interior.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls[0]).toEqual({
      sessionId: "session-1", endRow: 200n, maxRows: 100, gridEpoch: GRID_EPOCH,
    });
    expect(interior.painted.has(150)).toBe(true);
    head.controller.dispose();
    interior.controller.dispose();
  });

  test("a short interior response reports its actual retained floor row, and clears it", async () => {
    const h = harness({ total: 300, painted: INTERIOR_PAINTED, focus: 150, sessionId: "interior-floor" });
    rpcImpl = async (request) => response(120, Number(request.endRow), 300, {
      historyFloor: ScrollbackHistoryFloor.EVICTED,
    });

    h.controller.onUserScroll();
    await flushWork();
    expect(scrollbackHistoryFloor("interior-floor")).toEqual({ row: 120, reason: "evicted" });
    expect(h.painted.has(150)).toBe(true);
    expect(h.painted.has(119)).toBe(false);
    // The pager owns the floor VALUE and its lifetime; whether the head spacer
    // may drop its pending texture is the renderer's derivation, not a latch
    // here — this floor of 120 was proven by an INTERIOR page and says nothing
    // about the spacer, which stands over [0, sbBase).
    h.anchor.gridEpoch = "test-grid:1";
    h.controller.onFullFrame();
    expect(scrollbackHistoryFloor("interior-floor")).toBeNull();
    expect(h.floorRows).toEqual([120, 0]);
    h.controller.dispose();
  });

  test("find supersedes an obsolete scroll response before it can paint", async () => {
    const h = harness({
      total: 760,
      painted: Array.from({ length: 250 }, (_, index) => index + 500),
      focus: 755,
    });
    const stale = Promise.withResolvers<ScrollResponse>();
    let calls = 0;
    rpcImpl = (request) => ++calls === 1
      ? stale.promise
      : Promise.resolve(response(0, Number(request.endRow), 760));

    h.controller.onUserScroll();
    await flushWork();
    const ensured = h.controller.ensureRowPainted(100);
    await flushWork();
    stale.resolve(response(750, 756, 760));
    expect(await ensured).toBe(true);
    await flushWork();
    expect(h.painted.has(755)).toBe(false);
    expect(h.painted.has(100)).toBe(true);
    h.controller.dispose();
  });

  test("rejects stale responses, cancels suspended or rewound work, and accepts monotonic growth", async () => {
    const staleEpoch = harness({ total: 300, focus: 100 });
    rpcImpl = async (request) => response(0, Number(request.endRow), 300, { gridEpoch: "other:0" });
    staleEpoch.controller.onUserScroll();
    await flushWork();
    expect(staleEpoch.insertions).toHaveLength(0);
    expect(diagEvents.find((event) => event.guard === "epoch")).toBeDefined();

    const cancelled = harness({ total: 300, focus: 100 });
    const pending = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => pending.promise;
    cancelled.controller.onUserScroll();
    await flushWork();
    cancelled.controller.suspend();
    pending.resolve(response(0, 250, 300));
    await flushWork();
    expect(cancelled.insertions).toHaveLength(0);

    const rewound = harness({ total: 300, focus: 100 });
    const rewindResponse = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => rewindResponse.promise;
    rewound.controller.onUserScroll();
    await flushWork();
    rewound.anchor.total = 99;
    rewound.controller.onFullFrame();
    rewindResponse.resolve(response(0, 250, 300));
    await flushWork();
    expect(rewound.insertions).toHaveLength(0);

    const growing = harness({ total: 300, focus: 100 });
    const growingResponse = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => growingResponse.promise;
    growing.controller.onUserScroll();
    await flushWork();
    growing.anchor.total = 360;
    growingResponse.resolve(response(0, 250, 360));
    await flushWork();
    expect(growing.painted.has(100)).toBe(true);
    for (const scoped of [staleEpoch, cancelled, rewound, growing]) scoped.controller.dispose();
  });

  test("full frames do not prefetch and ensureRowPainted resolves only after insertion", async () => {
    const h = harness({ total: 300, focus: 100, bottom: false });
    h.controller.onFullFrame();
    await flushWork();
    expect(rpcCalls).toHaveLength(0);

    const pending = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => pending.promise;
    const ensured = h.controller.ensureRowPainted(100);
    await flushWork();
    expect(rpcCalls).toHaveLength(1);
    pending.resolve(response(0, 250, 300));
    expect(await ensured).toBe(true);
    expect(h.insertions.flat().some((row) => row.index === 100)).toBe(true);
    h.controller.dispose();
  });

  test("a wheel step pre-pays the rows above the viewport and the next step is free", async () => {
    const h = harness({ total: 5000, painted: Array.from({ length: 65 }, (_, index) => index + 4935), focus: 4934 });
    rpcImpl = async (request) => response(Number(request.endRow) - request.maxRows, Number(request.endRow), 5000);

    h.controller.onUserScroll();
    await flushWork();
    // The page ends at the blank edge the reader exposed and extends older, so
    // one round trip serves the viewport plus the rows it is scrolling toward.
    expect(rpcCalls.map((call) => [Number(call.endRow), call.maxRows])).toEqual([
      [4935, 250], [4685, 250], [4435, 250],
    ]);

    h.setFocus(4900); // the identical next wheel step is already painted
    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls).toHaveLength(3);

    h.setFocus(4600); // unpainted rows re-enter the read-ahead window
    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls[3]!.endRow).toBe(4185n);
    expect(rpcCalls[3]!.maxRows).toBe(250);
    h.controller.dispose();
  });

  test("scrolls during a wave add no request, one coalesce line, and one demand after", async () => {
    const h = harness({ total: 1000, focus: 900 });
    const first = Promise.withResolvers<ScrollResponse>();
    let calls = 0;
    rpcImpl = () => (++calls === 1 ? first.promise : new Promise<ScrollResponse>(() => undefined));

    h.controller.onUserScroll();
    await flushWork();
    h.setFocus(300);
    for (let step = 0; step < 3; step++) h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls.map((call) => [Number(call.endRow), call.maxRows])).toEqual([[901, 250]]);

    first.resolve(response(651, 901, 1000));
    await flushWork();
    expect(rpcCalls.map((call) => [Number(call.endRow), call.maxRows])).toEqual([[901, 250], [301, 250]]);

    for (let step = 0; step < 3; step++) h.controller.onUserScroll();
    await flushWork();
    // The owed edge reports once per wave; a fling raises ~60 events a second.
    expect(diagEvents.filter((event) => event.evt === "scrollback.demand_coalesced")).toEqual([
      { evt: "scrollback.demand_coalesced", sid: "session-1", kind: "scroll", focus: 51, start: 51, end: 301 },
    ]);
    h.controller.dispose();
  });

  test("a page that cannot splice retries bounded and stays armed for the reader", async () => {
    const h = harness({ total: 1000, focus: 600 });
    rpcImpl = async (request) => response(100, Number(request.endRow), 1000);

    h.controller.onUserScroll();
    await flushWork();
    // splicePage refuses an unpainted prefix: the identical demand stops there
    // until the retry interval elapses or the reader moves, whichever is first.
    expect(rpcCalls.map((call) => Number(call.endRow))).toEqual([601, 601, 601]);
    expect(h.insertions).toHaveLength(0);

    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls).toHaveLength(6);
    h.controller.dispose();
  });

  test("an exhausted identical-retry budget defers one re-arm instead of abandoning the gap", async () => {
    const h = harness({ total: 1000, focus: 600 });
    rpcImpl = async (request) => response(100, Number(request.endRow), 1000);

    h.controller.onUserScroll();
    await flushWork();
    const demands = diagEvents.filter((event) => String(event.evt).startsWith("scrollback.demand_"));
    expect(demands.map((event) => event.evt)).toEqual([
      "scrollback.demand_rearmed", "scrollback.demand_rearmed", "scrollback.demand_retry_deferred",
    ]);
    expect(demands.at(-1)).toEqual({
      evt: "scrollback.demand_retry_deferred", sid: "session-1",
      focus: 351, start: 351, end: 601, retries: 2, delay_ms: BACKFILL_RETRY_MS,
    });
    h.controller.dispose();
  });

  test("the reader's own rows paint when one page would span the painted base", async () => {
    // The live-stack state behind a permanently blank pane: head spacer
    // [0, 171), one gap element [171, 671), nothing painted, and a scrollbar
    // drag to the very top that raises ONE scroll event and never another.
    const h = harness({ total: 671, paintedBase: 171, painted: [], focus: 0 });
    rpcImpl = async (request) =>
      response(Number(request.endRow) - request.maxRows, Number(request.endRow), 671);

    h.controller.onUserScroll();
    await flushWork();
    // [0, 250) would cover the head spacer AND the gap above it, which the
    // renderer refuses, so the page stops at the base and the reader sees rows.
    expect(rpcCalls.map((call) => [Number(call.endRow), call.maxRows])).toEqual([[171, 171]]);
    expect(h.insertions.flat().map((row) => row.index)).toEqual(
      Array.from({ length: 171 }, (_, index) => index),
    );

    h.setFocus(200); // the reader keeps going, into the gap above the base
    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls.map((call) => [Number(call.endRow), call.maxRows])).toEqual(
      [[171, 171], [421, 250]],
    );
    expect(h.painted.has(200)).toBe(true);
    h.controller.dispose();
  });

  test("a spent budget keeps re-deriving on the retry cadence, one wave per interval", async () => {
    vi.useFakeTimers();
    const h = harness({ total: 1000, focus: 600 });
    rpcImpl = async (request) => response(100, Number(request.endRow), 1000);

    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls).toHaveLength(3);

    vi.advanceTimersByTime(BACKFILL_RETRY_MS - 1);
    await flushWork();
    expect(rpcCalls).toHaveLength(3);
    vi.advanceTimersByTime(1);
    await flushWork();
    // The reader parked at the top and never scrolled again: the pager owes
    // these rows, so the pager — not the next gesture — re-derives them.
    expect(rpcCalls).toHaveLength(4);

    for (let interval = 0; interval < 3; interval++) {
      vi.advanceTimersByTime(BACKFILL_RETRY_MS);
      await flushWork();
    }
    expect(rpcCalls).toHaveLength(7);
    expect(rpcCalls.every((call) => Number(call.endRow) === 601)).toBe(true);
    expect(diagEvents.filter((event) => event.evt === "scrollback.demand_retry_woke")).toEqual(
      Array.from({ length: 4 }, () => ({
        evt: "scrollback.demand_retry_woke", sid: "session-1",
        focus: 351, start: 351, end: 601, armed: true,
      })),
    );
    h.controller.dispose();
  });

  test("suspend and dispose cancel the deferred re-arm", async () => {
    vi.useFakeTimers();
    const suspended = harness({ total: 1000, focus: 600, sessionId: "suspended" });
    const disposed = harness({ total: 1000, focus: 600, sessionId: "disposed" });
    rpcImpl = async (request) => response(100, Number(request.endRow), 1000);

    suspended.controller.onUserScroll();
    disposed.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls).toHaveLength(6);

    suspended.controller.suspend();
    disposed.controller.dispose();
    vi.advanceTimersByTime(BACKFILL_RETRY_MS * 5);
    await flushWork();
    expect(rpcCalls).toHaveLength(6);
    suspended.controller.dispose();
  });

  test("a scroll event re-arms at once and supersedes the pending re-arm", async () => {
    vi.useFakeTimers();
    const h = harness({ total: 1000, focus: 600 });
    rpcImpl = async (request) => response(100, Number(request.endRow), 1000);

    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls).toHaveLength(3);

    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls).toHaveLength(6);

    // Seven, not eight: the gesture's own chain deferred exactly one timer and
    // the one it replaced is gone.
    vi.advanceTimersByTime(BACKFILL_RETRY_MS);
    await flushWork();
    expect(rpcCalls).toHaveLength(7);
    h.controller.dispose();
  });
});
