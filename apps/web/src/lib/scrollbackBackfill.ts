// Demand-driven cell history. Authoritative full frames contain only the live
// viewport plus a truthful sbBase spacer; scroll/find demand pages immutable
// epoch-addressed rows and prepends them at the exact absolute seam.

import { coordClient } from "../connect.ts";
import type { SessionsGetScrollbackCellsResponse } from "@roost/shared/proto/coordinator_pb";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { cellRowFromProto } from "@roost/shared/cell/cell-proto";
import type { CellRow } from "@roost/shared/cell";
import {
  MAX_HELD_SCROLLBACK_ROWS,
  type BackfillAnchor,
  type CellGridRenderer,
  type ReaderAnchor,
} from "./cellRenderer.ts";
import {
  backfillStateOf,
  SCROLLBACK_FLOOR_REASON,
} from "./scrollbackBackfillState.ts";
export {
  scrollbackBackfillRequestCount,
  scrollbackHistoryFloor,
  SCROLLBACK_FLOOR_REASON,
} from "./scrollbackBackfillState.ts";


// Rows per disjoint RPC page.
const BACKFILL_FETCH_ROWS = 1000;
// Rows per prependScrollback call, one per animation frame. This is the number
// the DOM cares about: a large splice builds thousands of row/span nodes
// synchronously at ~0.15ms/row (the deep-scrollback attach freeze — the whole
// UI stalls while history materializes). Decoupling the two means fewer round
// trips at exactly the same per-frame main-thread cost.
const BACKFILL_SPLICE_ROWS = 250;
// One retry after a transient RPC failure (coord 8s timeout, reconnect),
// then park until the next trigger.
const BACKFILL_RETRY_MS = 2000;
// Concurrent disjoint pages per demand wave.
const BACKFILL_CONCURRENCY = 3;



export interface ScrollbackBackfill {
  onFullFrame(): void;
  onUserScrollUp(): void;
  /** Cancel pending response and splice work without disposing the controller. */
  suspend(): void;
  /** Explicit find demand may page until the absolute row is painted. */
  ensureRowPainted(absIndex: number): Promise<boolean>;
  dispose(): void;
}

interface ValidatedChunk {
  rows: CellRow[];
  /** Earliest absolute row the worker still retains, when this page hit it. */
  retainedFloor: number | null;
  /** Which floor that was — "none" while the page was served in full. */
  floorReason: ScrollbackHistoryFloor;
}

/** Which fail-closed guard rejected a page, as reported on the diag channel.
 *  `epoch` is the one a core rebuild trips; the rest name a worker that answered
 *  a different question than the one asked. */
type ChunkGuard =
  | "epoch" | "cols" | "total"
  | "start_row" | "row_order" | "end_row"
  | "row_count" | "row_index";

export function createScrollbackBackfill(opts: {
  sessionId: string;
  renderer: () => CellGridRenderer | null;
  active: () => boolean;
}): ScrollbackBackfill {
  let epoch = 0;
  let activeLoop = -1;
  let activeAnchorRestore = -1;
  let disposed = false;
  let fullGridEpoch: string | null = null;
  let fullCols = 0;
  let fullTotal = -1;
  let retainedFloor = 0;

  const valid = (myEpoch: number): boolean =>
    !disposed && myEpoch === epoch && opts.active();

  const suspend = (): void => {
    epoch++;
    activeLoop = -1;
    activeAnchorRestore = -1;
  };

  const start = (): void => {
    if (
      disposed
      || !opts.active()
      || activeLoop === epoch
      || activeAnchorRestore === epoch
    ) return;
    const myEpoch = epoch;
    activeLoop = myEpoch;
    void loop(myEpoch);
  };

  function maybeStart(): void {
    if (disposed || !opts.active() || activeAnchorRestore === epoch) return;
    const renderer = opts.renderer();
    const anchor = renderer?.backfillAnchor();
    if (!renderer || !anchor || anchor.sbBase <= retainedFloor) return;
    if (
      anchor.total - anchor.sbBase >= MAX_HELD_SCROLLBACK_ROWS
      && !renderer.nearHistoryTop()
    ) return;
    start();
  }


  /** Paint newest-to-oldest; targeted pulls stop as soon as their row is visible. */
  async function spliceBatch(
    rows: readonly CellRow[],
    myEpoch: number,
    gridEpoch: string,
    throughRow?: number,
  ): Promise<boolean> {
    for (let end = rows.length; end > 0; end -= BACKFILL_SPLICE_ROWS) {
      if (!valid(myEpoch)) return false;
      const renderer = opts.renderer();
      const live = renderer?.backfillAnchor();
      if (!renderer || !live || live.gridEpoch !== gridEpoch) return false;
      const slice = rows.slice(Math.max(0, end - BACKFILL_SPLICE_ROWS), end);
      if (!valid(myEpoch)) return false;
      renderer.prependScrollback(slice);
      await new Promise<void>((resolve) =>
        typeof requestAnimationFrame === "function"
          ? requestAnimationFrame(() => resolve())
          : setTimeout(resolve, 0),
      );
      if (!valid(myEpoch)) return false;
      if (throughRow !== undefined && slice[0]!.index <= throughRow) return true;
    }
    return true;
  }


  /** Fail-closed page validation. Every rejection ALSO names its guard on the
   *  diag channel: a rejected page and "this session has no more history" are
   *  otherwise the same observation from outside — history just stops loading as
   *  the reader scrolls up, with nothing recorded anywhere. Bounded by
   *  construction: a rejection ends its wave, so there is at most one of these
   *  per issued RPC, and the payload is a fixed key set (no row content). */
  function validateChunk(
    resp: SessionsGetScrollbackCellsResponse,
    anchor: BackfillAnchor,
    endRow: number,
  ): ValidatedChunk | null {
    const startRow = Number(resp.startRow);
    const responseEnd = Number(resp.endRow);
    const total = Number(resp.scrollbackTotal);
    const expectedStart = Math.max(0, endRow - BACKFILL_FETCH_ROWS);
    const reject = (guard: ChunkGuard): null => {
      diag("scrollback.backfill_rejected", {
        sid: opts.sessionId,
        guard,
        requested_end: endRow,
        anchor_epoch: anchor.gridEpoch,
        response_epoch: resp.gridEpoch,
        anchor_cols: anchor.cols,
        response_cols: resp.cols,
        anchor_total: anchor.total,
        response_total: total,
        start_row: startRow,
        end_row: responseEnd,
        rows: resp.rows.length,
      });
      return null;
    };
    if (resp.gridEpoch !== anchor.gridEpoch) return reject("epoch");
    if (resp.cols !== anchor.cols) return reject("cols");
    if (total !== anchor.total) return reject("total");
    if (startRow < expectedStart) return reject("start_row");
    if (startRow > responseEnd) return reject("row_order");
    if (responseEnd !== endRow) return reject("end_row");
    const rows = resp.rows.map(cellRowFromProto);
    if (rows.length !== responseEnd - startRow) return reject("row_count");
    for (let i = 0; i < rows.length; i++) {
      if (rows[i]!.index !== startRow + i) return reject("row_index");
    }
    return {
      rows,
      retainedFloor: startRow > expectedStart ? startRow : null,
      floorReason: SCROLLBACK_FLOOR_REASON[resp.historyFloor] ?? "none",
    };
  }

  async function fetchChunk(
    myEpoch: number,
    anchor: BackfillAnchor,
    endRow: number,
  ): Promise<ValidatedChunk | null> {
    backfillStateOf(opts.sessionId).requests++;
    const resp = await coordClient.sessionsGetScrollbackCells({
      sessionId: opts.sessionId,
      endRow: BigInt(endRow),
      maxRows: BACKFILL_FETCH_ROWS,
      gridEpoch: anchor.gridEpoch,
    });
    if (!valid(myEpoch)) return null;
    return validateChunk(resp, anchor, endRow);
  }

  /** One writer for the proven floor: this controller's paging bound AND the
   *  document-level record the layered terminal probe reads. Monotonic within an
   *  epoch — the floor only rises — and the reason belongs to the page that
   *  raised it, so "history stopped loading here" is always attributable to
   *  either genuine eviction or a resize-bounded replay. */
  function noteFloor(chunk: ValidatedChunk): void {
    if (chunk.retainedFloor === null) return;
    retainedFloor = Math.max(retainedFloor, chunk.retainedFloor);
    const state = backfillStateOf(opts.sessionId);
    state.floor = retainedFloor;
    state.floorReason = chunk.floorReason;
  }

  async function loop(myEpoch: number): Promise<void> {
    try {
      let retried = false;
      while (valid(myEpoch)) {
        const renderer = opts.renderer();
        const anchor = renderer?.backfillAnchor();
        if (!renderer || !anchor || anchor.sbBase <= retainedFloor) return;
        const nearTop = renderer.nearHistoryTop();
        if (
          anchor.total - anchor.sbBase >= MAX_HELD_SCROLLBACK_ROWS
          && !nearTop
        ) return;

        const want = nearTop
          ? anchor.sbBase
          : Math.min(
              anchor.sbBase,
              MAX_HELD_SCROLLBACK_ROWS - (anchor.total - anchor.sbBase),
            );
        const waves = Math.min(
          BACKFILL_CONCURRENCY,
          Math.max(1, Math.ceil(want / BACKFILL_FETCH_ROWS)),
        );
        const ends: number[] = [];
        for (let k = 0; k < waves; k++) {
          const end = anchor.sbBase - k * BACKFILL_FETCH_ROWS;
          if (end <= 0) break;
          ends.push(end);
        }
        if (ends.length === 0) return;

        const pending = ends.map((end) => {
          backfillStateOf(opts.sessionId).requests++;
          return coordClient.sessionsGetScrollbackCells({
            sessionId: opts.sessionId,
            endRow: BigInt(end),
            maxRows: BACKFILL_FETCH_ROWS,
            gridEpoch: anchor.gridEpoch,
          }).then(
            (response) => response,
            () => "retry" as const,
          );
        });
        let transient = false;
        for (let k = 0; k < ends.length; k++) {
          const response = await pending[k]!;
          if (!valid(myEpoch)) return;
          if (response === "retry") {
            transient = true;
            break;
          }
          const chunk = validateChunk(response, anchor, ends[k]!);
          if (!chunk) return;
          noteFloor(chunk);
          const rows = chunk.rows;
          if (rows.length === 0) return;
          diag("scrollback.backfill", {
            sid: opts.sessionId,
            start_row: rows[0]!.index,
            end_row: ends[k]!,
            rows: rows.length,
            sb_base_after: rows[0]!.index,
            chunk: k,
            wave: ends.length,
          });
          if (!(await spliceBatch(rows, myEpoch, anchor.gridEpoch))) return;
          if (!valid(myEpoch)) return;
          const after = opts.renderer()?.backfillAnchor();
          if (
            !after
            || after.gridEpoch !== anchor.gridEpoch
            || after.sbBase !== rows[0]!.index
          ) return;
        }

        if (transient) {
          if (retried) return;
          retried = true;
          await new Promise<void>((resolve) => setTimeout(resolve, BACKFILL_RETRY_MS));
          if (!valid(myEpoch)) return;
          continue;
        }
        retried = false;
      }
    } finally {
      if (activeLoop === myEpoch) activeLoop = -1;
    }
  }

  async function ensureRowPainted(absIndex: number): Promise<boolean> {
    if (disposed || !opts.active()) return false;
    const myEpoch = epoch;
    for (;;) {
      if (!valid(myEpoch)) return false;
      const anchor = opts.renderer()?.backfillAnchor();
      if (!anchor || absIndex < 0 || absIndex >= anchor.total) return false;
      if (anchor.sbBase <= absIndex) return true;
      if (absIndex < retainedFloor) return false;
      let chunk: ValidatedChunk | null;
      try {
        chunk = await fetchChunk(myEpoch, anchor, anchor.sbBase);
      } catch {
        return false;
      }
      if (!valid(myEpoch) || !chunk) return false;
      noteFloor(chunk);
      if (chunk.rows.length === 0 || absIndex < retainedFloor) return false;
      if (!(await spliceBatch(chunk.rows, myEpoch, anchor.gridEpoch, absIndex))) return false;
      if (!valid(myEpoch)) return false;
    }
  }

  function restoreReaderAnchor(target: ReaderAnchor): void {
    if (disposed || !opts.active() || activeAnchorRestore === epoch) return;
    const myEpoch = epoch;
    activeAnchorRestore = myEpoch;
    void ensureRowPainted(target.row).then((painted) => {
      if (!painted || !valid(myEpoch)) return;
      opts.renderer()?.restoreReaderAnchor(target);
    }).finally(() => {
      if (activeAnchorRestore === myEpoch) activeAnchorRestore = -1;
    });
  }

  return {
    onFullFrame(): void {
      const renderer = opts.renderer();
      const anchor = renderer?.backfillAnchor();
      const identityChanged = !anchor
        || anchor.gridEpoch !== fullGridEpoch
        || anchor.cols !== fullCols
        || anchor.total !== fullTotal;
      if (identityChanged) {
        suspend();
        fullGridEpoch = anchor?.gridEpoch ?? null;
        fullCols = anchor?.cols ?? 0;
        fullTotal = anchor?.total ?? -1;
        retainedFloor = 0;
        // A new epoch's floor is unproven until a page comes back short in it, so
        // the probe must not keep showing the previous numbering's floor.
        const state = backfillStateOf(opts.sessionId);
        state.floor = 0;
        state.floorReason = "none";
      }
      if (!renderer || !opts.active()) return;
      const readerAnchor = renderer.readerAnchorForBackfill?.();
      if (readerAnchor) {
        restoreReaderAnchor(readerAnchor);
        return;
      }
      if (renderer.atBottom()) return;
      maybeStart();
    },
    onUserScrollUp(): void {
      maybeStart();
    },
    suspend,
    ensureRowPainted,
    dispose(): void {
      disposed = true;
      suspend();
    },
  };
}
