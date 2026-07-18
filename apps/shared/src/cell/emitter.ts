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
  seq: number;
  lastSbTotal: number;
  sentFull: boolean;
  cols: number;
  rows: number;
  alt: boolean;
}

export function initCellEmitState(): CellEmitState {
  return { seq: 0, lastSbTotal: 0, sentFull: false, cols: 0, rows: 0, alt: false };
}

/** Reframe (full frame) on first emit, force (attach/rebuild), dimension
 *  change, alt-screen toggle, or scrollback shrink (reset/eviction) — none
 *  expressible as an additive delta, so applyDelta (diff-grid.ts) can never
 *  mis-apply a delta across one of these transitions. Otherwise a dirty-row
 *  delta. `tailRows` caps a full frame's scrollback to the newest N lines
 *  (worker passes SB_SNAPSHOT_TAIL_ROWS; the [0, sbBase) rest is pulled via
 *  get-scrollback-cells); unset = complete history. */
export function nextCellFrame(
  core: TerminalCore, st: CellEmitState, force: boolean, tailRows?: number,
): { frame: CellGridFrame; state: CellEmitState } {
  const cols = core.getCols();
  const rows = core.getRows();
  const total = core.getScrollbackCount();
  const alt = core.usingAltScreen();
  const reframe = force || !st.sentFull
    || cols !== st.cols || rows !== st.rows
    || alt !== st.alt
    || total < st.lastSbTotal;
  const seq = st.seq + 1;
  const frame = reframe
    ? gridToCellFrame(core, seq, tailRows)
    : gridDeltaFrame(core, st.lastSbTotal, seq);
  return { frame, state: { seq, lastSbTotal: total, sentFull: true, cols, rows, alt } };
}
