// Demand-pages immutable terminal history at the visible or find-targeted gap.
// CellGridRenderer stays the only DOM/history owner, scrollbackDemandBounds owns
// the page geometry, and this controller fences one RPC wave by generation,
// epoch, columns, total and real painted coverage. Exactly one wave is in
// flight: a scroll raised mid-wave is coalesced, every settle re-derives the
// reader's gap, and an unchanged derivation backs off to the retry cadence.

import { coordClient } from "../connect.ts";
import { diag, type DiagKv } from "@roost/shared/diag";
import type { CellRow } from "@roost/shared/cell";
import { cellRowFromProto } from "@roost/shared/cell/cell-proto";
import type { SessionsGetScrollbackCellsResponse } from "@roost/shared/proto/coordinator_pb";
import { localTerminalTransport } from "../store/terminal-stream-transport.ts";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import type { CellGridRenderer } from "./cellRenderer.ts";
import {
  BACKFILL_AHEAD_ROWS,
  BACKFILL_IDENTICAL_RETRIES,
  type DemandBounds,
  findDemandBounds,
  scrollDemandBounds,
} from "./scrollbackDemandBounds.ts";
import {
  backfillStateOf,
  SCROLLBACK_FLOOR_REASON,
} from "./scrollbackBackfillState.ts";

export {
  scrollbackBackfillRequestCount,
  scrollbackHistoryFloor,
  SCROLLBACK_FLOOR_REASON,
} from "./scrollbackBackfillState.ts";

const BACKFILL_SPLICE_ROWS = 250;
/** Cadence of every pager retry: a failed fetch, and a spent identical-retry budget over a live gap. */
export const BACKFILL_RETRY_MS = 2000;

type DemandKind = "scroll" | "find";
type ChunkGuard =
  | "epoch" | "cols" | "total" | "start_row" | "end_row"
  | "row_count" | "row_index";
type ScrollbackRenderer = Pick<
  CellGridRenderer,
  | "followsBottom"
  | "backfillAnchor"
  | "hasPaintedScrollbackRange"
  | "insertHistoryPage"
  | "missingScrollbackRange"
  | "missingScrollbackRangeAtScroll"
  | "setHistoryFloor"
>;
/** The fields a page is validated against. The coordinator RPC and the local
 * worker socket answer the same query with the same shape. */
type ScrollbackPageResponse = Pick<
  SessionsGetScrollbackCellsResponse,
  "rows" | "cols" | "scrollbackTotal" | "startRow" | "endRow" | "gridEpoch" | "historyFloor"
>;

interface Demand extends DemandBounds {
  generation: number;
  kind: DemandKind;
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
  let scrollDemandOwed = false;
  let identicalRetries = 0;
  let deferredRearm: Timer | undefined;

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
    opts.renderer()?.setHistoryFloor(0);
  }

  /** The retry budget and the deferred re-arm it schedules are one state: a
   *  reader gesture, a changed derivation and a suspend all re-arm both. */
  function resetRetryBudget(): void {
    identicalRetries = 0;
    clearTimeout(deferredRearm);
    deferredRearm = undefined;
  }

  function suspend(): void {
    generation++;
    activeWave = null;
    scrollDemandOwed = false;
    resetRetryBudget();
  }

  function validatePage(response: ScrollbackPageResponse, demand: Demand): ValidatedPage | null {
    const start = Number(response.startRow);
    const end = Number(response.endRow);
    const total = Number(response.scrollbackTotal);
    /** One line names the guard that refused the page, the demand it was
     *  measured against and what came back, so a dropped wave is attributable. */
    const reject = (guard: ChunkGuard): null => {
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
    };
    if (response.gridEpoch !== demand.gridEpoch) return reject("epoch");
    if (response.cols !== demand.cols) return reject("cols");
    if (!Number.isSafeInteger(total) || total < demand.minimumTotal || total < demand.end) return reject("total");
    if (!Number.isSafeInteger(start) || start < 0 || start > demand.end) return reject("start_row");
    if (end !== demand.end) return reject("end_row");
    const rows = response.rows.map(cellRowFromProto);
    if (rows.length !== end - start) return reject("row_count");
    for (let offset = 0; offset < rows.length; offset++) {
      if (rows[offset]!.index !== start + offset) return reject("row_index");
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
    opts.renderer()?.setHistoryFloor(retainedFloor);
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

  /** Every demand state names the page it concerns in one flat shape, so a
   *  coalesce, a re-arm and a spent retry budget grep as one story. */
  function demandDiag(evt: string, page: DemandBounds, extra: DiagKv): void {
    diag(evt, { sid: opts.sessionId, focus: page.focus, start: page.start, end: page.end, ...extra });
  }

  function raiseDemand(kind: DemandKind, bounds: DemandBounds | null, readerIntent: boolean): Promise<boolean> {
    if (!bounds || !opts.active()) return Promise.resolve(false);
    const existing = activeWave;
    // Only the owed edge of a gesture reports, because a fling raises ~60
    // scroll events per second and the coalesce line names one wave.
    if (readerIntent && !scrollDemandOwed) {
      scrollDemandOwed = true;
      if (existing) demandDiag("scrollback.demand_coalesced", existing.demand, { kind: existing.demand.kind });
    }
    if (existing) {
      // Depth stays one wave: a scroll never orphans a page the worker already
      // read and the wire already carried, because the settle re-derives it.
      if (kind === "scroll") return existing.promise;
      if (existing.demand.kind === kind && existing.demand.focus === bounds.focus) return existing.promise;
    }
    const anchor = opts.renderer()?.backfillAnchor();
    if (!anchor) return Promise.resolve(false);
    const demand: Demand = {
      ...bounds,
      generation: ++generation,
      kind,
      gridEpoch: anchor.gridEpoch,
      cols: anchor.cols,
      minimumTotal: anchor.total,
    };
    const wave: ActiveWave = { demand, promise: Promise.resolve(false) };
    activeWave = wave;
    wave.promise = runDemand(demand).finally(() => {
      // A preempted wave no longer owns the pager: only the live one re-arms.
      if (activeWave !== wave) return;
      activeWave = null;
      rearmAfterWave(demand);
    });
    return wave.promise;
  }

  /** The page the reader's CURRENT scroll position demands, or null when the
   *  pager owes nothing: listener, settle and deferred re-arm all derive here. */
  function liveScrollDemand(): DemandBounds | null {
    const renderer = disposed || !opts.active() ? null : opts.renderer();
    if (!renderer || renderer.followsBottom()) return null;
    const target = renderer.missingScrollbackRangeAtScroll(BACKFILL_AHEAD_ROWS);
    const anchor = renderer.backfillAnchor();
    return target && anchor ? scrollDemandBounds(target, retainedFloor, anchor.sbBase) : null;
  }

  /** The identical-retry budget fences a hot loop of back-to-back waves; it is
   *  not permission to leave the reader's own visible rows blank, so a spent
   *  budget over a live gap derives again one interval later instead. */
  function deferRearm(next: DemandBounds): void {
    if (deferredRearm !== undefined) return;
    demandDiag("scrollback.demand_retry_deferred", next, { retries: identicalRetries, delay_ms: BACKFILL_RETRY_MS });
    deferredRearm = setTimeout(() => {
      deferredRearm = undefined;
      const live = liveScrollDemand();
      demandDiag("scrollback.demand_retry_woke", live ?? next, { armed: live !== null });
      if (live) void raiseDemand("scroll", live, false);
    }, BACKFILL_RETRY_MS);
  }

  /** `splicePage` reports false on benign paths and the reader keeps reading
   *  while a page lands, so every settle re-derives the live demand instead of
   *  waiting for the next scroll event. An unchanged derivation relaunches a
   *  bounded number of times, then falls back to the retry cadence. */
  function rearmAfterWave(settled: Demand): void {
    scrollDemandOwed = false;
    const next = liveScrollDemand();
    if (!next) { resetRetryBudget(); return; }
    const same = next.focus === settled.focus && next.start === settled.start && next.end === settled.end;
    if (!same) resetRetryBudget();
    if (same && identicalRetries >= BACKFILL_IDENTICAL_RETRIES) { deferRearm(next); return; }
    if (same) identicalRetries++;
    demandDiag("scrollback.demand_rearmed", next, {
      settled_focus: settled.focus, settled_start: settled.start, settled_end: settled.end, identical: same,
    });
    void raiseDemand("scroll", next, false);
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
      // A real gesture re-arms the budget and supersedes the deferred re-arm.
      resetRetryBudget();
      void raiseDemand("scroll", liveScrollDemand(), true);
    },
    suspend,
    ensureRowPainted(absIndex: number): Promise<boolean> {
      const renderer = opts.renderer();
      if (!renderer || !opts.active()) return Promise.resolve(false);
      if (renderer.hasPaintedScrollbackRange(absIndex, absIndex + 1)) return Promise.resolve(true);
      const gap = renderer.missingScrollbackRange(absIndex);
      const anchor = renderer.backfillAnchor();
      if (!gap || !anchor) return Promise.resolve(false);
      return raiseDemand("find", findDemandBounds(gap, absIndex, retainedFloor, anchor.sbBase), false);
    },
    dispose(): void {
      disposed = true;
      suspend();
    },
  };
}
