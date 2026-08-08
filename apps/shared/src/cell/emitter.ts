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
import {
  gridToCellFrame, gridDeltaFrame, scrollbackShift, scrollbackTailSig, nearScrollbackCap,
} from "./grid-to-cells.ts";
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
  /** Lines this core's ring has evicted — the origin of the monotonic index
   *  space (see grid-to-cells.ts). Only ever grows within one core's life. */
  sbDropped: number;
  /** Identity probe for the newest retained lines at the last emit; recovers
   *  the eviction count the saturated ring's pinned row count cannot report. */
  tailSig: string;
}

export function initCellEmitState(gridEpochBase: string): CellEmitState {
  return {
    gridEpochBase, gridEpochRevision: 0,
    seq: 0, lastSbTotal: 0, sentFull: false, cols: 0, rows: 0, alt: false, sbDropped: 0, tailSig: "",
  };
}

export function cellGridEpoch(state: CellEmitState): string {
  return `${state.gridEpochBase}:${state.gridEpochRevision}`;
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
  // Below the ring cap the count delta already tells us nothing was evicted.
  // AT the cap it pins, so the count can no longer see lines scrolling off and
  // absolute indices silently re-alias (measured: 500 lines pushed, append=[],
  // absolute row 0 went LINE-78 -> LINE-578). Recover the shift explicitly —
  // but only where eviction is possible at all (nearScrollbackCap), because the
  // pinned-count gate below is also true for every non-scrolling delta, i.e.
  // for ordinary typing, which paid the probe's ~1200 WASM reads per keystroke.
  const nearCap = nearScrollbackCap(total);
  const shift = nearCap && total === st.lastSbTotal - st.sbDropped && total > 0
    ? scrollbackShift(core, st.tailSig)
    : 0;
  const sbDropped = st.sbDropped + (shift ?? 0);
  const monoTotal = sbDropped + total;
  const semanticReframe = !force && st.sentFull && (
    cols !== st.cols || rows !== st.rows
    || alt !== st.alt
    || monoTotal < st.lastSbTotal
    || shift === null
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
      // scrollbackShift(core, "") is 0, so the first emit after crossing the
      // floor is a no-op shift rather than a spurious reframe.
      sbDropped, tailSig: nearCap ? scrollbackTailSig(core) : "",
    },
  };
}
