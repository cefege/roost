import { afterAll, beforeAll, beforeEach, describe, expect, mock, test } from "bun:test";
import type { CellRow } from "@roost/shared/cell";
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
  rows: Array<{
    index: number;
    spans: Array<{ text: string; fg: number; bg: number; flags: number }>;
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

// The Connect client must be mocked before the controller module is evaluated.
const { createScrollbackBackfill } = await import("../src/lib/scrollbackBackfill.ts");

const GRID_EPOCH = "test-grid:0";

function response(startRow: number, endRow: number, total = 1000): ScrollResponse {
  return {
    cols: 80,
    scrollbackTotal: BigInt(total),
    startRow: BigInt(startRow),
    endRow: BigInt(endRow),
    gridEpoch: GRID_EPOCH,
    rows: Array.from({ length: endRow - startRow }, (_, offset) => ({
      index: startRow + offset,
      spans: [{ text: `row-${startRow + offset}`, fg: 256, bg: 256, flags: 0 }],
    })),
  };
}

function harness(bottom: boolean) {
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
      if (anchor.sbBase === 0) paintComplete.resolve();
    },
  } as unknown as CellGridRenderer;
  const controller = createScrollbackBackfill({
    sessionId: "session-1",
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
});
