// Shared fixture for the scrollback demand-paging suite: a renderer stand-in
// modelling ABSOLUTE painted rows and exact missing intervals, plus the page
// response builder and the microtask drain those tests step. Not a renderer —
// no DOM, no layout, no scrollTop — so painted coverage is answerable without
// a paint. The suite imports createScrollbackBackfill itself and passes it in:
// a top-level await here would bind the controller before its mocks exist.

import { afterAll, beforeAll } from "bun:test";
import type { CellRow } from "@roost/protocol/cell";
import { ScrollbackHistoryFloor } from "@roost/protocol/proto/coordinator_pb";
import type { createScrollbackBackfill } from "../src/lib/scrollbackBackfill.ts";

export type ScrollRequest = {
  sessionId: string;
  endRow: bigint;
  maxRows: number;
  gridEpoch: string;
};
export type ScrollResponse = {
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
export type CreateScrollbackBackfill = typeof createScrollbackBackfill;

export interface BackfillHarnessOptions {
  total?: number;
  painted?: readonly number[];
  /** The renderer's `_paintedSbBase`: the head spacer covers `[0, base)` and
   *  explicit gap elements cover the unpainted rows above it. */
  paintedBase?: number;
  focus?: number | null;
  sessionId?: string;
  bottom?: boolean;
}

export const GRID_EPOCH = "test-grid:0";
/** Painted head and tail around one interior hole [100, 200). */
export const INTERIOR_PAINTED = [
  ...Array.from({ length: 100 }, (_, index) => index),
  ...Array.from({ length: 100 }, (_, index) => index + 200),
];

export function response(
  startRow: number,
  endRow: number,
  total: number,
  options: {
    gridEpoch?: string;
    historyFloor?: ScrollbackHistoryFloor;
  } = {},
): ScrollResponse {
  return {
    cols: 80,
    scrollbackTotal: BigInt(total),
    startRow: BigInt(startRow),
    endRow: BigInt(endRow),
    gridEpoch: options.gridEpoch ?? GRID_EPOCH,
    historyFloor: options.historyFloor ?? ScrollbackHistoryFloor.UNSPECIFIED,
    rows: Array.from({ length: endRow - startRow }, (_, offset) => ({
      index: startRow + offset,
      spans: [{ text: `row-${startRow + offset}`, columns: 5, fg: 256, bg: 256, flags: 0 }],
    })),
  };
}

export function createBackfillHarness(
  create: CreateScrollbackBackfill,
  options: BackfillHarnessOptions = {},
) {
  const anchor = {
    sbBase: options.paintedBase ?? options.painted?.[0] ?? options.total ?? 0,
    cols: 80,
    total: options.total ?? 760,
    gridEpoch: GRID_EPOCH,
  };
  const painted = new Set(options.painted ?? []);
  const insertions: CellRow[][] = [];
  const floorRows: number[] = [];
  let visibleFocus = options.focus ?? null;
  const bottom = options.bottom ?? false;
  const active = true;
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
    followsBottom: () => bottom,
    missingScrollbackRange: missing,
    setHistoryFloor: (row: number) => { floorRows.push(row); },
    // One-row viewport at the focus row, widened upward exactly like the real
    // helper: the bottom-most missing interval inside the window wins.
    missingScrollbackRangeAtScroll: (aheadRows = 0) => {
      const top = Math.max(0, (visibleFocus ?? 0) - Math.max(0, aheadRows));
      for (let row = visibleFocus ?? -1; row >= top; row--) {
        const gap = missing(row);
        if (gap) return { ...gap, focusRow: Math.max(gap.start, top), visibleEnd: row + 1 };
      }
      return null;
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
      // One placeholder per page, exactly like `_insertPageIntoPlaceholder`:
      // the head spacer stands over [0, sbBase) and a gap element over each
      // missing interval above it, and no insert spans two of them.
      if (start < anchor.sbBase && end > anchor.sbBase) return false;
      for (let offset = 0; offset < rows.length; offset++) {
        if (rows[offset]!.index !== start + offset) return false;
      }
      insertions.push(Array.from(rows));
      for (const row of rows) painted.add(row.index);
      if (start < anchor.sbBase) anchor.sbBase = start;
      return true;
    },
  };
  const controller = create({
    sessionId: options.sessionId ?? "session-1",
    renderer: () => renderer,
    active: () => active,
  });
  return {
    anchor, painted, insertions, floorRows, controller,
    setFocus(next: number | null) { visibleFocus = next; },
  };
}

export async function flushWork(): Promise<void> {
  for (let index = 0; index < 128; index++) await Promise.resolve();
}

const originalAnimationFrame = globalThis.requestAnimationFrame;
function defineAnimationFrame(value: unknown): void {
  if (value === undefined) Reflect.deleteProperty(globalThis, "requestAnimationFrame");
  else Object.defineProperty(globalThis, "requestAnimationFrame", { configurable: true, value });
}

/** Installed by the suite, never at import time: bun caches this module, so an
 *  import-time hook would bind to whichever file imported it first. */
export function installScrollbackFrameLifecycle(): void {
  beforeAll(() => defineAnimationFrame((callback: FrameRequestCallback) => {
    queueMicrotask(() => callback(0));
    return 1;
  }));
  afterAll(() => defineAnimationFrame(originalAnimationFrame));
}
