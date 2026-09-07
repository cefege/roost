// Shared fixture for the terminal-find controller suites: a renderer+backfill
// harness around createTerminalFind and the captured debounce clock those
// suites step. Used by terminalFindController.test.ts and
// terminalFindController-paging.test.ts; the mocked search RPC and its spies
// come from ./terminalFindController-search-spy.ts.
// Each suite passes its own dynamically imported createTerminalFind: a
// top-level await inside THIS module would let a suite's tests run while its
// body is still suspended, so the controller import belongs to the suite.

import { afterEach, beforeEach } from "bun:test";
import { ScrollbackHistoryFloor, SearchStopReason } from "@roost/shared/proto/coordinator_pb";
import {
  cancellationRequests,
  requests,
  setSearchRpc,
  signals,
  type SearchResponse,
} from "./terminalFindController-search-spy.ts";
import type { createTerminalFind } from "../src/lib/terminalFindController.ts";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { FindHit } from "../src/lib/cellRow.ts";
import type { ScrollbackBackfill } from "../src/lib/scrollbackBackfill.ts";

export {
  cancellationRequests,
  requests,
  setSearchRpc,
  signals,
  type SearchRequest,
  type SearchResponse,
} from "./terminalFindController-search-spy.ts";

// Capture debounce timers so tests never sleep through FIND_DEBOUNCE_MS.
const pendingTimers = new Map<number, () => void>();
let nextTimerId = 1;
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;

export const EPOCH_A = "grid-a:0";
export const EPOCH_B = "grid-b:0";

interface ReplyOptions { stop?: SearchStopReason; start?: number; end?: number; next?: number }

export function reply(rows: number[], gridEpoch: string, options: ReplyOptions = {}): SearchResponse {
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

export type CreateTerminalFind = typeof createTerminalFind;

export function createFindHarness(create: CreateTerminalFind) {
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
  const find = create({
    sessionId: "session-1", renderer: () => renderer, backfill: () => backfill,
  });
  return {
    anchor, jumps, pulled, published, find,
    last(): Published { return published[published.length - 1] ?? { rows: [], active: null }; },
  };
}

// The page chain awaits one RPC per page, so a full 32-page chain needs more
// microtask turns than any single page does.
export async function settle(): Promise<void> {
  for (let idx = 0; idx < 300; idx++) await Promise.resolve();
}

export async function fireDebounce(): Promise<void> {
  const due = Array.from(pendingTimers.values());
  pendingTimers.clear();
  for (const fn of due) fn();
  await settle();
}

// Each suite installs the captured clock and the per-test spy reset for its own
// file; bun caches this module, so import-time hooks would bind to one file only.
// Install and restore per TEST, not per suite: the unit tier runs every web spec
// in one process, so a suite-scoped restore would hand the real clock back while
// another file's suite is still pending.
export function installTerminalFindTestLifecycle(): void {
  beforeEach(() => {
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
    requests.length = 0;
    signals.length = 0;
    cancellationRequests.length = 0;
    pendingTimers.clear();
    setSearchRpc(async () => reply([], EPOCH_A));
  });

  afterEach(() => {
    Object.defineProperty(globalThis, "setTimeout", { configurable: true, value: realSetTimeout });
    Object.defineProperty(globalThis, "clearTimeout", { configurable: true, value: realClearTimeout });
  });
}
