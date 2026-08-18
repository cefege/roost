// Cell emit decision (R11). Pure given the core: decides full-vs-delta and
// advances the per-channel emit state. The worker (session-manager) holds
// one CellEmitState per session and calls nextCellFrame after each PTY chunk
// (and force=true on attach/rebuild). Extracted here so the full/delta/seq
// logic is unit-tested against a core, not buried in SessionManager.
//
// CONTRACT: the caller MUST call core.clearDirty() AFTER nextCellFrame so the
// next delta carries only newly-dirtied rows. nextCellFrame does not mutate
// the core.

import type { TerminalCore } from "@wterm/core";
import { gridToCellFrame, gridDeltaFrame } from "./grid-to-cells.ts";
import type { CellGridFrame } from "./types.ts";

export interface CellEmitState {
  /** Opaque identity base for one worker-side grid numbering lifetime. */
  gridEpochBase: string;
  /** Increments only when a non-forced semantic reframe invalidates row identity. */
  gridEpochRevision: number;
  seq: number;
  /** Last emitted scrollbackTotal, in MONOTONIC index space (sbDropped + retained). */
  lastSbTotal: number;
  sentFull: boolean;
  cols: number;
  rows: number;
  alt: boolean;
  /** Base of Roost's monotonic index space for the CURRENT core instance. A
   *  resize rebuilds the core and replays the raw ring, which restarts the
   *  core's own discarded counter at 0 while the SPA's absolute row indices must
   *  not rewind; the rebuild pins that difference here (session-resize-capture).
   *  0 for the core a session spawns with. */
  sbOrigin: number;
  /** Eviction origin at the last emit: sbOrigin plus the core's authoritative
   *  discarded count, read ONCE so a frame's sbBase, scrollbackTotal and append
   *  range all describe the same observation of the ring. */
  sbDropped: number;
}

export function initCellEmitState(gridEpochBase: string): CellEmitState {
  return {
    gridEpochBase, gridEpochRevision: 0,
    seq: 0, lastSbTotal: 0, sentFull: false, cols: 0, rows: 0, alt: false,
    sbOrigin: 0, sbDropped: 0,
  };
}

export function cellGridEpoch(state: CellEmitState): string {
  return `${state.gridEpochBase}:${state.gridEpochRevision}`;
}

/** Lines the ring has evicted, in Roost's monotonic index space — the origin
 *  every absolute row index is measured from (see grid-to-cells.ts).
 *
 *  @wterm/core 0.3.4 counts discards itself, so this is a READ. Roost used to
 *  infer it: at saturation getScrollbackCount() pins, so the emitter
 *  re-identified the previously-newest lines by content hash to recover how far
 *  the ring had slid. That probe cost ~1200 WASM reads per emit near the cap,
 *  went blind past a 256-line scan window, and could alias two identical tails —
 *  and each of those failure modes silently re-aliased absolute row indices,
 *  which is the L11 "history mis-splices" class.
 *
 *  Callers reading history BETWEEN emits (the backfill and search RPCs) must use
 *  this rather than the last emitted `sbDropped`, whose offsets are stale by
 *  whatever the ring has evicted since. */
export function scrollbackOrigin(core: TerminalCore, state: CellEmitState): number {
  const discarded = core.getScrollbackDiscardedCount;
  if (discarded === undefined) {
    throw new Error(
      "terminal core does not implement getScrollbackDiscardedCount(): the scrollback "
      + "origin cannot be authoritative and every absolute history index would re-alias",
    );
  }
  return state.sbOrigin + discarded.call(core);
}

/** Reframe on first emit, force, or a semantic grid transition. A force-only
 * claim snapshot keeps the held epoch; a non-forced dimension/alt/rewind/ring
 * transition after the initial frame advances the epoch because old absolute
 * rows no longer identify the same grid. `tailRows` bounds full-frame history. */
export function nextCellFrame(
  core: TerminalCore, st: CellEmitState, force: boolean, tailRows?: number,
): { frame: CellGridFrame; state: CellEmitState } {
  const cols = core.getCols();
  const rows = core.getRows();
  const total = core.getScrollbackCount();
  const alt = core.usingAltScreen();
  const sbDropped = scrollbackOrigin(core, st);
  const monoTotal = sbDropped + total;
  const semanticReframe = !force && st.sentFull && (
    cols !== st.cols || rows !== st.rows
    || alt !== st.alt
    // A vertical grow pops rows back OUT of history into the viewport without
    // discarding anything, so the monotonic total shrinks; so does a reset.
    || monoTotal < st.lastSbTotal
    // The ring evicted PAST what the client holds — a whole ring's worth of
    // lines inside one coalesce window. A delta's append starts at the retained
    // floor, so [lastSbTotal, sbDropped) would never reach the client and its
    // history would splice a hole. Those lines are gone from the ring either
    // way; an honest reframe is the only truthful frame left.
    || sbDropped > st.lastSbTotal
  );
  const reframe = force || !st.sentFull || semanticReframe;
  const gridEpochRevision = st.gridEpochRevision + (semanticReframe ? 1 : 0);
  const gridEpoch = `${st.gridEpochBase}:${gridEpochRevision}`;
  const seq = st.seq + 1;
  const frame = reframe
    ? gridToCellFrame(core, seq, gridEpoch, tailRows, sbDropped)
    : gridDeltaFrame(core, st.lastSbTotal, seq, gridEpoch, sbDropped);
  return {
    frame,
    state: {
      gridEpochBase: st.gridEpochBase, gridEpochRevision,
      seq, lastSbTotal: monoTotal, sentFull: true, cols, rows, alt,
      sbOrigin: st.sbOrigin, sbDropped,
    },
  };
}
