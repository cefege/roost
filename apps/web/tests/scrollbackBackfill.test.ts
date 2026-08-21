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

// `diag` is a no-op unless ROOST_DIAG was set BEFORE @roost/shared/diag first
// evaluated, and bun shares one module registry across test files — so any file
// that loads diag earlier in the profile bakes the disabled emitter in. Mock the
// emitter instead of racing that gate: order-independent, and the always-on
// signal channel plus the sink setters stay real for every other module.
const diagEvents: Array<Record<string, unknown>> = [];
const realDiag = await import("@roost/shared/diag");
mock.module("@roost/shared/diag", () => ({
  ...realDiag,
  diag(evt: string, kv: Record<string, unknown>) { diagEvents.push({ evt, ...kv }); },
}));

// The Connect client and the diag emitter must both be mocked before the
// controller module is evaluated.
const {
  createScrollbackBackfill,
  scrollbackHistoryFloor,
} = await import("../src/lib/scrollbackBackfill.ts");

const GRID_EPOCH = "test-grid:0";

function response(
  startRow: number,
  endRow: number,
  total = 1000,
  historyFloor = ScrollbackHistoryFloor.UNSPECIFIED,
): ScrollResponse {
  return {
    cols: 80,
    scrollbackTotal: BigInt(total),
    startRow: BigInt(startRow),
    endRow: BigInt(endRow),
    gridEpoch: GRID_EPOCH,
    historyFloor,
    rows: Array.from({ length: endRow - startRow }, (_, offset) => {
      const text = `row-${startRow + offset}`;
      return { index: startRow + offset, spans: [{ text, columns: text.length, fg: 256, bg: 256, flags: 0 }] };
    }),
  };
}

/** `stopAt` is the sbBase the paint is expected to settle on: 0 for a wave that
 *  reaches the beginning, the retained floor for one that is clamped short. */
function harness(bottom: boolean, opts?: { sessionId?: string; stopAt?: number }) {
  const sessionId = opts?.sessionId ?? "session-1";
  const stopAt = opts?.stopAt ?? 0;
  const anchor = { sbBase: 1000, cols: 80, total: 1000, gridEpoch: GRID_EPOCH };
  const prepends: CellRow[][] = [];
  let painted: CellRow[] = [];
  const paintComplete = Promise.withResolvers<void>();
  let active = true;
  const renderer = {
    backfillAnchor: () => ({ ...anchor }),
    nearHistoryTop: () => true,
    atBottom: () => bottom,
    prependScrollback(rows: readonly CellRow[]) {
      const copy = Array.from(rows);
      prepends.push(copy);
      painted = copy.concat(painted);
      anchor.sbBase -= copy.length;
      if (anchor.sbBase === stopAt) paintComplete.resolve();
    },
  } as unknown as CellGridRenderer;
  const controller = createScrollbackBackfill({
    sessionId,
    renderer: () => renderer,
    active: () => active,
  });
  return {
    anchor,
    prepends,
    painted: () => painted,
    complete: paintComplete.promise,
    controller,
  };
}

async function flushWork(): Promise<void> {
  for (let i = 0; i < 4; i++) await Promise.resolve();
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
  rpcImpl = async () => response(0, 1000);
});

describe("ScrollbackBackfill demand and cancellation", () => {
  test("a bottom full frame performs no history RPC", async () => {
    const h = harness(true);
    h.controller.onFullFrame();
    await flushWork();
    expect(rpcCalls).toHaveLength(0);
    expect(h.prepends).toHaveLength(0);
    h.controller.dispose();
  });

  test("an off-bottom scroll requests a disjoint epoch page and prepends it", async () => {
    const h = harness(false);
    h.controller.onUserScrollUp();
    await h.complete;

    expect(rpcCalls[0]).toEqual({
      sessionId: "session-1",
      endRow: 1000n,
      maxRows: 1000,
      gridEpoch: GRID_EPOCH,
    });
    expect(h.anchor.sbBase).toBe(0);
    expect(h.painted().map((row) => row.index)).toEqual(
      Array.from({ length: 1000 }, (_, index) => index),
    );
    h.controller.dispose();
  });

  test("a same-identity full frame does not cancel pending history", async () => {
    const pending = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => pending.promise;
    const h = harness(false);

    h.controller.onFullFrame();
    expect(rpcCalls).toHaveLength(1);
    h.controller.onFullFrame();
    expect(rpcCalls).toHaveLength(1);

    pending.resolve(response(0, 1000));
    await h.complete;
    expect(h.anchor.sbBase).toBe(0);
    expect(h.painted()).toHaveLength(1000);
    h.controller.dispose();
  });

  test("an incompatible full fetches through the reader anchor before restoring it", async () => {
    const order: string[] = [];
    const pending = Promise.withResolvers<ScrollResponse>();
    const restoreComplete = Promise.withResolvers<void>();
    rpcImpl = () => {
      order.push("request");
      return pending.promise;
    };
    const anchor = {
      sbBase: 1000,
      cols: 80,
      total: 1000,
      gridEpoch: GRID_EPOCH,
    };
    let readerAnchor: { row: number; offsetPx: number } | null = null;
    const renderer = {
      backfillAnchor: () => ({ ...anchor }),
      readerAnchorForBackfill: () => readerAnchor,
      atBottom: () => true,
      prependScrollback(rows: readonly CellRow[]) {
        order.push("prepend");
        anchor.sbBase = rows[0]!.index;
      },
      restoreReaderAnchor(restored: { row: number; offsetPx: number }) {
        order.push("restore");
        expect(restored).toEqual({ row: 800, offsetPx: 4 });
        restoreComplete.resolve();
        return true;
      },
    } as unknown as CellGridRenderer;
    const controller = createScrollbackBackfill({
      sessionId: "session-reader-anchor",
      renderer: () => renderer,
      active: () => true,
    });

    controller.onFullFrame();
    anchor.gridEpoch = "test-grid:1";
    readerAnchor = { row: 800, offsetPx: 4 };
    controller.onFullFrame();

    expect(rpcCalls[0]).toEqual({
      sessionId: "session-reader-anchor",
      endRow: 1000n,
      maxRows: 1000,
      gridEpoch: "test-grid:1",
    });
    expect(order).toEqual(["request"]);

    pending.resolve({ ...response(0, 1000), gridEpoch: "test-grid:1" });
    await flushWork();
    await restoreComplete.promise;
    expect(order).toEqual(["request", "prepend", "restore"]);
    controller.dispose();
  });

  test("a short page paints the retained suffix and parks at its floor", async () => {
    rpcImpl = async (request) =>
      request.endRow === 1000n
        ? response(76, 1000)
        : response(76, 76);
    const h = harness(false);

    h.controller.onUserScrollUp();
    for (let attempt = 0; attempt < 8 && h.anchor.sbBase !== 76; attempt++) {
      await flushWork();
    }
    await flushWork();

    expect(h.anchor.sbBase).toBe(76);
    expect(h.painted()).toHaveLength(924);
    expect(h.painted()[0]!.index).toBe(76);
    expect(h.painted()[923]!.index).toBe(999);
    expect(rpcCalls).toHaveLength(1);

    h.controller.onUserScrollUp();
    await flushWork();
    expect(rpcCalls).toHaveLength(1);
    h.controller.dispose();
  });

  test("suspend discards a pending response and later demand starts fresh", async () => {
    const firstResponse = Promise.withResolvers<ScrollResponse>();
    rpcImpl = () => firstResponse.promise;
    const h = harness(false);

    h.controller.onUserScrollUp();
    expect(rpcCalls).toHaveLength(1);
    h.controller.suspend();
    firstResponse.resolve(response(0, 1000));
    await flushWork();
    expect(h.prepends).toHaveLength(0);

    rpcImpl = async () => response(0, 1000);
    h.controller.onUserScrollUp();
    await h.complete;
    expect(rpcCalls).toHaveLength(2);
    expect(h.anchor.sbBase).toBe(0);
    expect(h.painted()).toHaveLength(1000);
    h.controller.dispose();
  });

  // A rejected page and "this session has no more history" look identical from
  // outside: history simply stops loading as the reader scrolls up. The reason
  // must be recoverable after the fact, or a rebuild racing a backfill wave is
  // undiagnosable.
  test("a page from another epoch paints nothing and names the guard it failed", async () => {
    rpcImpl = async () => ({ ...response(0, 1000), gridEpoch: "other-grid:0" });
    const h = harness(false);

    h.controller.onUserScrollUp();
    await flushWork();

    // Fail-closed behaviour is unchanged: nothing painted, the wave stops.
    expect(h.prepends).toHaveLength(0);
    expect(h.anchor.sbBase).toBe(1000);
    expect(rpcCalls).toHaveLength(1);

    const rejected = diagEvents.filter((e) => e.evt === "scrollback.backfill_rejected");
    expect(rejected).toHaveLength(1);
    expect(rejected[0]).toMatchObject({
      sid: "session-1",
      guard: "epoch",
      requested_end: 1000,
      anchor_epoch: GRID_EPOCH,
      response_epoch: "other-grid:0",
    });
    h.controller.dispose();
  });

  // A blank region at the top of history is the SAME observation whether those
  // rows are gone forever or were merely unreachable by a resize-forced replay.
  // The reason has to survive the page that proved the floor, or the top of
  // history is permanently unattributable.
  test("a page clamped at the retained floor records the floor AND why it is there", async () => {
    rpcImpl = async () => response(400, 1000, 1000, ScrollbackHistoryFloor.RESIZE_REPLAY);
    const h = harness(false, { sessionId: "session-replay-floor", stopAt: 400 });

    h.controller.onUserScrollUp();
    await h.complete;
    await flushWork();

    // Paging stops at the floor instead of re-asking for rows the worker just
    // said it does not have.
    expect(h.anchor.sbBase).toBe(400);
    expect(rpcCalls).toHaveLength(1);
    expect(scrollbackHistoryFloor("session-replay-floor")).toEqual({
      row: 400,
      reason: "resize_replay",
    });
    h.controller.dispose();
  });

  test("genuine eviction reads differently at the same floor row, and a new epoch clears it", async () => {
    rpcImpl = async () => response(400, 1000, 1000, ScrollbackHistoryFloor.EVICTED);
    const h = harness(false, { sessionId: "session-evicted-floor", stopAt: 400 });

    h.controller.onUserScrollUp();
    await h.complete;
    await flushWork();

    expect(scrollbackHistoryFloor("session-evicted-floor")).toEqual({
      row: 400,
      reason: "evicted",
    });

    // A rebuild renumbers history, so the previous epoch's floor says nothing
    // about the new one until a page comes back short in it.
    h.anchor.total = 2000;
    h.controller.onFullFrame();
    expect(scrollbackHistoryFloor("session-evicted-floor")).toBeNull();
    h.controller.dispose();
  });

  test("a page served in full claims no floor at all", async () => {
    const h = harness(false, { sessionId: "session-no-floor" });

    h.controller.onUserScrollUp();
    await h.complete;

    expect(h.anchor.sbBase).toBe(0);
    expect(scrollbackHistoryFloor("session-no-floor")).toBeNull();
    h.controller.dispose();
  });
});
