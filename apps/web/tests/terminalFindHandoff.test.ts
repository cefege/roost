// Pane-local validation for coordinates handed off by global content search.
// The terminal reruns a literal query against its live epoch and may prefer a
// coordinate only when that fresh result contains the exact epoch/row/column.

import { afterAll, beforeAll, beforeEach, describe, expect, mock, test } from "bun:test";
import { ScrollbackHistoryFloor, SearchStopReason } from "@roost/shared/proto/coordinator_pb";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { FindHit } from "../src/lib/cellRow.ts";
import type { ScrollbackBackfill } from "../src/lib/scrollbackBackfill.ts";

interface SearchRequest {
  readonly sessionId: string;
  readonly gridEpoch: string;
  readonly query: string;
  readonly caseSensitive: boolean;
  readonly regex: boolean;
}

interface SearchResponse {
  readonly matches: Array<{ row: bigint; col: number; len: number; preview: string }>;
  readonly truncated: boolean;
  readonly scrollbackTotal: bigint;
  readonly cols: number;
  readonly gridEpoch: string;
  readonly scannedStartRow: bigint;
  readonly scannedEndRow: bigint;
  readonly historyFloor: ScrollbackHistoryFloor;
  readonly stopReason: SearchStopReason;
}

const EPOCH_A = "grid-a:0";
const EPOCH_B = "grid-b:0";
const requests: SearchRequest[] = [];
let rpcResponse: SearchResponse;

mock.module("../src/connect.ts", () => ({
  coordClient: {
    async sessionsSearchScrollback(request: SearchRequest) {
      requests.push(request);
      return rpcResponse;
    },
    async sessionsCancelScrollbackSearch() {
      return {};
    },
  },
}));

const { createTerminalFind } = await import("../src/lib/terminalFindController.ts");

function response(rows: readonly number[], gridEpoch = EPOCH_A): SearchResponse {
  return {
    matches: rows.map((row) => ({ row: BigInt(row), col: 3, len: 6, preview: `row ${row}` })),
    truncated: false,
    scrollbackTotal: 2_000n,
    cols: 80,
    gridEpoch,
    scannedStartRow: 0n,
    scannedEndRow: 2_000n,
    historyFloor: ScrollbackHistoryFloor.UNSPECIFIED,
    stopReason: SearchStopReason.COMPLETE,
  };
}

function harness() {
  const jumps: number[] = [];
  const activeMatches: Array<{ row: number; col: number } | null> = [];
  const renderer = {
    backfillAnchor: () => ({ sbBase: 500, cols: 80, total: 2_000, gridEpoch: EPOCH_A }),
    setFindHighlights(
      _hits: ReadonlyMap<number, FindHit[]>,
      active: { row: number; col: number } | null,
    ) {
      activeMatches.push(active);
    },
    scrollToScrollbackRow(row: number) {
      jumps.push(row);
    },
  } as unknown as CellGridRenderer;
  const backfill = {
    async ensureRowPainted() {
      return true;
    },
  } as unknown as ScrollbackBackfill;
  const find = createTerminalFind({
    sessionId: "session-a",
    renderer: () => renderer,
    backfill: () => backfill,
  });
  return { find, jumps, activeMatches };
}

const pendingTimers = new Map<number, () => void>();
let nextTimer = 1;
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;

beforeAll(() => {
  Object.defineProperty(globalThis, "setTimeout", {
    configurable: true,
    value: (callback: () => void) => {
      const timer = nextTimer++;
      pendingTimers.set(timer, callback);
      return timer;
    },
  });
  Object.defineProperty(globalThis, "clearTimeout", {
    configurable: true,
    value: (timer?: number) => {
      if (timer !== undefined) pendingTimers.delete(timer);
    },
  });
});

afterAll(() => {
  Object.defineProperty(globalThis, "setTimeout", { configurable: true, value: realSetTimeout });
  Object.defineProperty(globalThis, "clearTimeout", { configurable: true, value: realClearTimeout });
});

beforeEach(() => {
  requests.length = 0;
  pendingTimers.clear();
  rpcResponse = response([1_500, 900]);
});

async function fireSearch(): Promise<void> {
  const callbacks = [...pendingTimers.values()];
  pendingTimers.clear();
  for (const callback of callbacks) callback();
  for (let idx = 0; idx < 20; idx++) await Promise.resolve();
}

describe("global result to pane-local find handoff", () => {
  test("selects an exact coordinate only after a fresh current-epoch literal result", async () => {
    const pane = harness();
    pane.find.toggleRegex();
    pane.find.setQuery("Needle.*", {
      literal: true,
      caseSensitive: true,
      preferredMatch: { gridEpoch: EPOCH_A, row: 900n, col: 3 },
    });
    await fireSearch();

    expect(requests[0]).toMatchObject({
      query: "Needle.*",
      caseSensitive: true,
      regex: false,
      gridEpoch: EPOCH_A,
    });
    expect(pane.find.index()).toBe(1);
    expect(pane.jumps).toEqual([900]);
    expect(pane.activeMatches.at(-1)).toEqual({ row: 900, col: 3 });
    pane.find.dispose();
  });

  test("ignores stale-epoch and absent coordinates while preserving fresh results", async () => {
    const staleEpoch = harness();
    staleEpoch.find.setQuery("needle", {
      literal: true,
      preferredMatch: { gridEpoch: EPOCH_B, row: 900n, col: 3 },
    });
    await fireSearch();
    expect(staleEpoch.find.matches().map((match) => match.row)).toEqual([900, 1_500]);
    expect(staleEpoch.find.index()).toBe(2);
    expect(staleEpoch.jumps).toEqual([1_500]);
    staleEpoch.find.dispose();

    const absentRow = harness();
    absentRow.find.setQuery("needle", {
      literal: true,
      preferredMatch: { gridEpoch: EPOCH_A, row: 1_200n, col: 3 },
    });
    await fireSearch();
    expect(absentRow.find.matches().map((match) => match.row)).toEqual([900, 1_500]);
    expect(absentRow.find.index()).toBe(2);
    expect(absentRow.jumps).toEqual([1_500]);
    absentRow.find.dispose();
  });
});
