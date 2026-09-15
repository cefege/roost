// Demand-pages immutable terminal history at the visible or find-targeted gap.
// CellGridRenderer remains the only DOM/history owner; this controller fences
// one RPC wave by generation, epoch, columns, total, and real painted coverage.

import { coordClient } from "../connect.ts";
import { diag } from "@roost/shared/diag";
import type { CellRow } from "@roost/shared/cell";
import { cellRowFromProto } from "@roost/shared/cell/cell-proto";
import type { SessionsGetScrollbackCellsResponse } from "@roost/shared/proto/coordinator_pb";
import { localTerminalTransport } from "../store/terminal-stream-transport.ts";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import type { CellGridRenderer } from "./cellRenderer.ts";
import {
  backfillStateOf,
  SCROLLBACK_FLOOR_REASON,
} from "./scrollbackBackfillState.ts";

export {
  scrollbackBackfillRequestCount,
  scrollbackHistoryFloor,
  SCROLLBACK_FLOOR_REASON,
} from "./scrollbackBackfillState.ts";

const BACKFILL_FETCH_ROWS = 1000;
const BACKFILL_SPLICE_ROWS = 250;
const BACKFILL_RETRY_MS = 2000;

type DemandKind = "scroll" | "find";
type ChunkGuard =
  | "epoch" | "cols" | "total" | "start_row" | "end_row"
  | "row_count" | "row_index";
type ScrollbackRenderer = Pick<
  CellGridRenderer,
  | "atBottom"
  | "backfillAnchor"
  | "hasPaintedScrollbackRange"
  | "insertHistoryPage"
  | "missingScrollbackRange"
  | "missingScrollbackRangeAtScroll"
>;
/** The fields a page is validated against. The coordinator RPC and the local
 * worker socket answer the same query with the same shape. */
type ScrollbackPageResponse = Pick<
  SessionsGetScrollbackCellsResponse,
  "rows" | "cols" | "scrollbackTotal" | "startRow" | "endRow" | "gridEpoch" | "historyFloor"
>;

interface Demand {
  generation: number;
  kind: DemandKind;
  focus: number;
  start: number;
  end: number;
  gridEpoch: string;
  cols: number;
  minimumTotal: number;
}

interface ActiveWave {
  demand: Demand;
  promise: Promise<boolean>;
}

interface ValidatedPage {
  rows: CellRow[];
  start: number;
  end: number;
  floorReason: ScrollbackHistoryFloor;
}

export interface ScrollbackBackfill {
  onFullFrame(): void;
  onUserScroll(): void;
  suspend(): void;
  ensureRowPainted(absIndex: number): Promise<boolean>;
  dispose(): void;
}

export function createScrollbackBackfill(opts: {
  sessionId: string;
  renderer: () => ScrollbackRenderer | null;
  active: () => boolean;
}): ScrollbackBackfill {
  let generation = 0;
  let activeWave: ActiveWave | null = null;
  let disposed = false;
  let frameEpoch: string | null = null;
  let frameCols = 0;
  let frameTotal = -1;
  let retainedFloor = 0;

  function isCurrent(demand: Demand): boolean {
    if (
      disposed
      || !opts.active()
      || activeWave?.demand !== demand
      || demand.generation !== generation
    ) return false;
    const anchor = opts.renderer()?.backfillAnchor();
    if (!anchor) return false;
    return anchor.gridEpoch === demand.gridEpoch
      && anchor.cols === demand.cols
      && anchor.total >= demand.minimumTotal
      && anchor.total >= demand.end;
  }

  function clearFloor(): void {
    retainedFloor = 0;
    const state = backfillStateOf(opts.sessionId);
    state.floor = 0;
    state.floorReason = "none";
  }

  function suspend(): void {
    generation++;
    activeWave = null;
  }

  function rejectPage(
    guard: ChunkGuard,
    response: ScrollbackPageResponse,
    demand: Demand,
    total: number,
    start: number,
    end: number,
  ): null {
    diag("scrollback.backfill_rejected", {
      sid: opts.sessionId,
      guard,
      requested_start: demand.start,
      requested_end: demand.end,
      response_epoch: response.gridEpoch,
      response_cols: response.cols,
      response_total: total,
      start_row: start,
      end_row: end,
      rows: response.rows.length,
    });
    return null;
  }

  function validatePage(
    response: ScrollbackPageResponse,
    demand: Demand,
  ): ValidatedPage | null {
    const start = Number(response.startRow);
    const end = Number(response.endRow);
    const total = Number(response.scrollbackTotal);
    if (response.gridEpoch !== demand.gridEpoch) {
      return rejectPage("epoch", response, demand, total, start, end);
    }
    if (response.cols !== demand.cols) {
      return rejectPage("cols", response, demand, total, start, end);
    }
    if (!Number.isSafeInteger(total) || total < demand.minimumTotal || total < demand.end) {
      return rejectPage("total", response, demand, total, start, end);
    }
    if (!Number.isSafeInteger(start) || start < 0 || start > demand.end) {
      return rejectPage("start_row", response, demand, total, start, end);
    }
    if (end !== demand.end) {
      return rejectPage("end_row", response, demand, total, start, end);
    }
    const rows = response.rows.map(cellRowFromProto);
    if (rows.length !== end - start) {
      return rejectPage("row_count", response, demand, total, start, end);
    }
    for (let offset = 0; offset < rows.length; offset++) {
      if (rows[offset]!.index !== start + offset) {
        return rejectPage("row_index", response, demand, total, start, end);
      }
    }
    return {
      rows,
      start,
      end,
      floorReason: SCROLLBACK_FLOOR_REASON[response.historyFloor] ?? "none",
    };
  }

  function noteFloor(page: ValidatedPage, demand: Demand): void {
    if (page.start <= demand.start) return;
    retainedFloor = Math.max(retainedFloor, page.start);
    const state = backfillStateOf(opts.sessionId);
    state.floor = retainedFloor;
    state.floorReason = page.floorReason;
  }

  async function fetchPage(demand: Demand): Promise<ValidatedPage | null> {
    backfillStateOf(opts.sessionId).requests++;
    const query = {
      sessionId: opts.sessionId,
      endRow: BigInt(demand.end),
      maxRows: demand.end - demand.start,
      gridEpoch: demand.gridEpoch,
    };
    // History pages follow the session's live transport, so a local pane can
    // still page its own worker's history with the coordinator unreachable.
    const local = localTerminalTransport();
    const response = local?.ownsSession(opts.sessionId)
      ? await local.requestScrollback(query)
      : await coordClient.sessionsGetScrollbackCells(query);
    if (!isCurrent(demand)) return null;
    return validatePage(response, demand);
  }

  async function waitForAnimationFrame(): Promise<void> {
    await new Promise<void>((resolve) => {
      if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => resolve());
      else setTimeout(resolve, 0);
    });
  }

  async function splicePage(page: ValidatedPage, demand: Demand): Promise<boolean> {
    const firstRequestedOffset = Math.max(0, demand.start - page.start);
    const initialRenderer = opts.renderer();
    if (
      page.start < demand.start
      && (
        !initialRenderer
        || !initialRenderer.hasPaintedScrollbackRange(page.start, demand.start)
      )
    ) return false;
    let cursor = page.rows.length;
    while (cursor > firstRequestedOffset) {
      if (!isCurrent(demand)) return false;
      const renderer = opts.renderer();
      const newest = page.rows[cursor - 1]!;
      if (renderer?.hasPaintedScrollbackRange(newest.index, newest.index + 1)) {
        cursor--;
        continue;
      }
      const gap = renderer?.missingScrollbackRange(newest.index);
      if (!renderer || !gap) return false;
      const start = Math.max(
        demand.start,
        page.start,
        gap.start,
        newest.index + 1 - BACKFILL_SPLICE_ROWS,
      );
      const from = start - page.start;
      const rows = page.rows.slice(from, cursor);
      const inserted = renderer.insertHistoryPage(rows, false);
      if (!inserted && !renderer.hasPaintedScrollbackRange(rows[0]!.index, newest.index + 1)) {
        return false;
      }
      cursor = from;
      await waitForAnimationFrame();
      if (!isCurrent(demand)) return false;
    }
    return opts.renderer()?.hasPaintedScrollbackRange(demand.focus, demand.focus + 1) ?? false;
  }

  async function runDemand(demand: Demand): Promise<boolean> {
    let retried = false;
    for (;;) {
      if (!isCurrent(demand)) return false;
      let page: ValidatedPage | null;
      try {
        page = await fetchPage(demand);
      } catch {
        if (retried || !isCurrent(demand)) return false;
        retried = true;
        await new Promise<void>((resolve) => setTimeout(resolve, BACKFILL_RETRY_MS));
        continue;
      }
      if (!page || !isCurrent(demand)) return false;
      noteFloor(page, demand);
      return splicePage(page, demand);
    }
  }

  function startDemand(kind: DemandKind, focus: number): Promise<boolean> {
    const renderer = opts.renderer();
    if (!renderer || !opts.active()) return Promise.resolve(false);
    if (renderer.hasPaintedScrollbackRange(focus, focus + 1)) return Promise.resolve(true);
    const gap = renderer.missingScrollbackRange(focus);
    const anchor = renderer.backfillAnchor();
    if (!gap || !anchor) return Promise.resolve(false);
    const lower = Math.max(gap.start, retainedFloor);
    // A top-visible focus must fill the bounded head page; deeper gaps advance
    // from the focus so one request never skips its visible target.
    const start = focus - lower < BACKFILL_FETCH_ROWS ? lower : focus;
    const end = Math.min(gap.end, start + BACKFILL_FETCH_ROWS);
    if (start >= end) return Promise.resolve(false);
    const existing = activeWave;
    if (existing?.demand.kind === "find" && kind === "scroll") return existing.promise;
    if (existing && existing.demand.kind === kind && existing.demand.focus === focus) {
      return existing.promise;
    }
    const demand: Demand = {
      generation: ++generation,
      kind,
      focus,
      start,
      end,
      gridEpoch: anchor.gridEpoch,
      cols: anchor.cols,
      minimumTotal: anchor.total,
    };
    const wave: ActiveWave = { demand, promise: Promise.resolve(false) };
    activeWave = wave;
    wave.promise = runDemand(demand).finally(() => {
      if (activeWave === wave) activeWave = null;
    });
    return wave.promise;
  }

  return {
    onFullFrame(): void {
      const renderer = opts.renderer();
      const anchor = renderer?.backfillAnchor() ?? null;
      const totalRewound = anchor !== null && (
        (frameTotal >= 0 && anchor.total < frameTotal)
        || (activeWave !== null && anchor.total < activeWave.demand.minimumTotal)
      );
      const identityChanged = !anchor
        || anchor.gridEpoch !== frameEpoch
        || anchor.cols !== frameCols
        || totalRewound;
      if (identityChanged) {
        suspend();
        frameEpoch = anchor?.gridEpoch ?? null;
        frameCols = anchor?.cols ?? 0;
        frameTotal = anchor?.total ?? -1;
        clearFloor();
      } else if (anchor) {
        frameTotal = Math.max(frameTotal, anchor.total);
      }
    },
    onUserScroll(): void {
      const renderer = opts.renderer();
      if (!renderer || renderer.atBottom()) return;
      const gap = renderer.missingScrollbackRangeAtScroll();
      if (gap) void startDemand("scroll", gap.focusRow);
    },
    suspend,
    ensureRowPainted(absIndex: number): Promise<boolean> {
      return startDemand("find", absIndex);
    },
    dispose(): void {
      disposed = true;
      suspend();
    },
  };
}
