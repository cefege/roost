// Scrollback demand-paging tests exercise request fencing without a browser DOM.
// The renderer harness models absolute painted rows and exact missing intervals.
// DOM insertion and placeholder ownership remain covered by renderer DOM tests.

import { afterAll, beforeAll, beforeEach, describe, expect, mock, test } from "bun:test";
import type { CellRow } from "@roost/shared/cell";
import { ScrollbackHistoryFloor } from "@roost/shared/proto/coordinator_pb";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";

type ScrollRequest = {
  sessionId: string;
  endRow: bigint;
  maxRows: number;
  gridEpoch: string;
};
type ScrollResponse = {
  cols: number;
  scrollbackTotal: bigint;
  startRow: bigint;
  endRow: bigint;
  gridEpoch: string;
  historyFloor: ScrollbackHistoryFloor;
  rows: Array<{
    index: number;
    spans: Array<{ text: string; columns: number; fg: number; bg: number; flags: number }>;
  }>;
};

const rpcCalls: ScrollRequest[] = [];
let rpcImpl: (request: ScrollRequest) => Promise<ScrollResponse>;

mock.module("../src/connect.ts", () => ({
  coordClient: {
    sessionsGetScrollbackCells(request: ScrollRequest) {
      rpcCalls.push(request);
      return rpcImpl(request);
    },
  },
}));

const diagEvents: Array<Record<string, unknown>> = [];
const realDiag = await import("@roost/shared/diag");
mock.module("@roost/shared/diag", () => ({
  ...realDiag,
  diag(evt: string, kv: Record<string, unknown>) { diagEvents.push({ evt, ...kv }); },
}));

const {
  createScrollbackBackfill,
  scrollbackHistoryFloor,
} = await import("../src/lib/scrollbackBackfill.ts");

const GRID_EPOCH = "test-grid:0";

function response(
  startRow: number,
  endRow: number,
  total: number,
  options: {
    gridEpoch?: string;
    cols?: number;
    historyFloor?: ScrollbackHistoryFloor;
  } = {},
): ScrollResponse {
  return {
    cols: options.cols ?? 80,
    scrollbackTotal: BigInt(total),
    startRow: BigInt(startRow),
    endRow: BigInt(endRow),
    gridEpoch: options.gridEpoch ?? GRID_EPOCH,
    historyFloor: options.historyFloor ?? ScrollbackHistoryFloor.UNSPECIFIED,
    rows: Array.from({ length: endRow - startRow }, (_, offset) => {
      const index = startRow + offset;
      return {
        index,
        spans: [{ text: `row-${index}`, columns: 5, fg: 256, bg: 256, flags: 0 }],
      };
    }),
  };
}

function harness(options: {
  total?: number;
  painted?: readonly number[];
  focus?: number | null;
  sessionId?: string;
  bottom?: boolean;
  readerAnchor?: { row: number; offsetPx: number };
} = {}) {
  const anchor = {
    sbBase: options.painted?.[0] ?? options.total ?? 0,
    cols: 80,
    total: options.total ?? 760,
    gridEpoch: GRID_EPOCH,
  };
  const painted = new Set(options.painted ?? []);
  const insertions: CellRow[][] = [];
  let visibleFocus = options.focus ?? null;
  let bottom = options.bottom ?? false;
  let active = true;
  const restoredAnchors: Array<{ row: number; offsetPx: number }> = [];
  function missing(absIndex: number): { start: number; end: number } | null {
    if (!Number.isInteger(absIndex) || absIndex < 0 || absIndex >= anchor.total || painted.has(absIndex)) {
      return null;
    }
    let start = absIndex;
    let end = absIndex + 1;
    while (start > 0 && !painted.has(start - 1)) start--;
    while (end < anchor.total && !painted.has(end)) end++;
    return { start, end };
  }

  const renderer = {
    backfillAnchor: () => ({ ...anchor }),
    atBottom: () => bottom,
    missingScrollbackRange: missing,
    missingScrollbackRangeAtScroll: () => {
      const focus = visibleFocus;
      const gap = focus === null ? null : missing(focus);
      return gap === null || focus === null ? null : { ...gap, focusRow: focus };
    },
    hasPaintedScrollbackRange(start: number, end: number) {
      if (!Number.isInteger(start) || !Number.isInteger(end) || start < 0 || start >= end || end > anchor.total) {
        return false;
      }
      for (let index = start; index < end; index++) if (!painted.has(index)) return false;
      return true;
    },
    insertHistoryPage(rows: readonly CellRow[]) {
      const start = rows[0]?.index;
      const end = start === undefined ? undefined : start + rows.length;
      const gap = start === undefined ? null : missing(start);
      if (!gap || end === undefined || end > gap.end) return false;
      for (let offset = 0; offset < rows.length; offset++) {
        if (rows[offset]!.index !== start + offset) return false;
      }
      insertions.push(Array.from(rows));
      for (const row of rows) painted.add(row.index);
      if (start < anchor.sbBase) anchor.sbBase = start;
      return true;
    },
    readerAnchorForBackfill: () => options.readerAnchor ? { ...options.readerAnchor } : null,
    restoreReaderAnchor(anchor: { row: number; offsetPx: number }) {
      restoredAnchors.push(anchor);
      return true;
    },
  };
  const controller = createScrollbackBackfill({
    sessionId: options.sessionId ?? "session-1",
    renderer: () => renderer,
    active: () => active,
  });
  return {
    anchor,
    painted,
    insertions,
    restoredAnchors,
    controller,
    setFocus(next: number | null) { visibleFocus = next; },
    setBottom(next: boolean) { bottom = next; },
    setActive(next: boolean) { active = next; },
  };
}

async function flushWork(): Promise<void> {
  for (let index = 0; index < 24; index++) await Promise.resolve();
}

const originalAnimationFrame = globalThis.requestAnimationFrame;
beforeAll(() => {
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    configurable: true,
    value: (callback: FrameRequestCallback) => {
      queueMicrotask(() => callback(0));
      return 1;
    },
  });
});
afterAll(() => {
  if (originalAnimationFrame) {
    Object.defineProperty(globalThis, "requestAnimationFrame", {
      configurable: true,
      value: originalAnimationFrame,
    });
  } else {
    Reflect.deleteProperty(globalThis, "requestAnimationFrame");
  }
});

beforeEach(() => {
  rpcCalls.length = 0;
  diagEvents.length = 0;
  rpcImpl = async (request) => response(
    Number(request.endRow) - request.maxRows,
    Number(request.endRow),
    760,
  );
});

describe("ScrollbackBackfill arbitrary gap paging", () => {
  test("fills a bounded short tail gap from its reader focus", async () => {
    const h = harness({
      total: 760,
      painted: Array.from({ length: 250 }, (_, index) => index + 500),
      focus: 755,
    });
    rpcImpl = async (request) => response(750, Number(request.endRow), 760);

    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls[0]).toEqual({
      sessionId: "session-1", endRow: 760n, maxRows: 10, gridEpoch: GRID_EPOCH,
    });
    expect(h.insertions[0]!.map((row) => row.index)).toEqual(
      Array.from({ length: 10 }, (_, index) => index + 750),
    );
    expect(h.painted.has(759)).toBe(true);
    h.controller.dispose();
  });

  test("pages head and interior gaps without inferring coverage from sbBase", async () => {
    const head = harness({ total: 300, focus: 50 });
    rpcImpl = async (request) => response(0, Number(request.endRow), 300);
    head.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls[0]!.endRow).toBe(300n);
    expect(rpcCalls[0]!.maxRows).toBe(300);

    rpcCalls.length = 0;
    const painted = [
      ...Array.from({ length: 100 }, (_, index) => index),
      ...Array.from({ length: 100 }, (_, index) => index + 200),
    ];
    const interior = harness({ total: 300, painted, focus: 150 });
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

  test("bounds a deep-gap request to a forward page containing the reader", async () => {
    const h = harness({ total: 12_000, focus: 10_000 });
    rpcImpl = async (request) => response(
      Number(request.endRow) - request.maxRows,
      Number(request.endRow),
      12_000,
    );

    h.controller.onUserScroll();
    await flushWork();

    expect(rpcCalls[0]).toEqual({
      sessionId: "session-1", endRow: 11_000n, maxRows: 1_000, gridEpoch: GRID_EPOCH,
    });
    expect(h.painted.has(10_000)).toBe(true);
    h.controller.dispose();
  });

  test("a short interior response records its actual retained floor, not the gap edge", async () => {
    const painted = [
      ...Array.from({ length: 100 }, (_, index) => index),
      ...Array.from({ length: 100 }, (_, index) => index + 200),
    ];
    const h = harness({ total: 300, painted, focus: 150, sessionId: "interior-floor" });
    rpcImpl = async (request) => response(
      120,
      Number(request.endRow),
      300,
      { historyFloor: ScrollbackHistoryFloor.EVICTED },
    );

    h.controller.onUserScroll();
    await flushWork();
    expect(scrollbackHistoryFloor("interior-floor")).toEqual({ row: 120, reason: "evicted" });
    expect(h.painted.has(150)).toBe(true);
    expect(h.painted.has(119)).toBe(false);
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


  test("a user scroll supersedes a delayed reader-anchor restore", async () => {
    const h = harness({ total: 760, focus: 100, readerAnchor: { row: 700, offsetPx: 0 } });
    const delayedRestore = Promise.withResolvers<ScrollResponse>();
    let calls = 0;
    rpcImpl = (request) => ++calls === 1
      ? delayedRestore.promise
      : Promise.resolve(response(0, Number(request.endRow), 760));

    h.controller.onFullFrame();
    await flushWork();
    h.controller.onUserScroll();
    await flushWork();
    expect(rpcCalls).toHaveLength(2);

    delayedRestore.resolve(response(0, 760, 760));
    await flushWork();
    expect(h.restoredAnchors).toEqual([]);
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
    pending.resolve(response(0, 101, 300));
    await flushWork();
    expect(cancelled.insertions).toHaveLength(0);

    const rewound = harness({ total: 300, focus: 100 });
    const rewindResponse = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => rewindResponse.promise;
    rewound.controller.onUserScroll();
    await flushWork();
    rewound.anchor.total = 99;
    rewound.controller.onFullFrame();
    rewindResponse.resolve(response(0, 101, 300));
    await flushWork();
    expect(rewound.insertions).toHaveLength(0);

    const growing = harness({ total: 300, focus: 100 });
    const growingResponse = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => growingResponse.promise;
    growing.controller.onUserScroll();
    await flushWork();
    growing.anchor.total = 360;
    growingResponse.resolve(response(0, 300, 360));
    await flushWork();
    expect(growing.painted.has(100)).toBe(true);
    staleEpoch.controller.dispose();
    cancelled.controller.dispose();
    rewound.controller.dispose();
    growing.controller.dispose();
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
    pending.resolve(response(0, 300, 300));
    expect(await ensured).toBe(true);
    expect(h.insertions.flat().some((row) => row.index === 100)).toBe(true);
    h.controller.dispose();
  });
});
