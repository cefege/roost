// Pure page geometry for demand-paged terminal history: the absolute rows one
// RPC wave asks for, given a missing interval, the window the reader exposed,
// the retention floor the worker proved and the painted head base the DOM can
// splice on either side of. scrollbackBackfill.ts owns the renderer lookups,
// the fencing and the splice; nothing here reads DOM, module state or the
// wire, so a bound is reproducible from its arguments alone.

import type { CellHistoryRange, CellHistoryScrollTarget } from "./cellHistoryRanges.ts";

/** Rows one wave fetches — one worker `SB_BLOCK`. */
export const BACKFILL_FETCH_ROWS = 250;
/** Rows above the viewport the trigger window covers, so a demand is raised
 *  before the reader reaches the blank rows it asks for. */
export const BACKFILL_AHEAD_ROWS = 500;
/** Waves an unchanged derivation may relaunch back to back before it falls
 *  back to the retry cadence. */
export const BACKFILL_IDENTICAL_RETRIES = 2;

/** One demand's half-open absolute row range, plus the row it must end painted. */
export interface DemandBounds {
  focus: number;
  start: number;
  end: number;
}

/** The page the reader's exposed gap demands: it ends at the newest missing
 *  row in the window and extends older, pre-paying what the reader scrolls
 *  toward; with under a page older than that edge it extends newer instead. */
export function scrollDemandBounds(
  target: CellHistoryScrollTarget,
  retainedFloor: number,
  paintedBase: number,
): DemandBounds | null {
  const lower = Math.max(target.start, retainedFloor);
  // A page lands in exactly ONE placeholder: below the painted base the
  // renderer splices the head spacer, above it the single gap element covering
  // the page, and no insert spans both. Above the base a page always lies
  // inside one gap — a painted row can never sit inside a missing interval —
  // so the base is the only boundary, and the side holding the newest missing
  // row the window exposed wins: the reader's own rows paint on this wave.
  const exposed = Math.max(lower, target.visibleEnd - 1);
  const aboveBase = exposed >= paintedBase;
  const floorRow = aboveBase ? Math.max(lower, paintedBase) : lower;
  const ceilRow = aboveBase ? target.end : Math.min(target.end, paintedBase);
  const end = Math.min(ceilRow, Math.max(target.visibleEnd, floorRow + BACKFILL_FETCH_ROWS));
  const start = Math.max(floorRow, end - BACKFILL_FETCH_ROWS);
  return start < end ? { focus: Math.max(start, target.focusRow), start, end } : null;
}

/** A match needs the context newer than itself, so a find page advances from
 *  its focus row; a top-visible focus still fills the bounded head page. */
export function findDemandBounds(
  gap: CellHistoryRange,
  focus: number,
  retainedFloor: number,
  paintedBase: number,
): DemandBounds | null {
  const lower = Math.max(gap.start, retainedFloor);
  // The same one-placeholder rule, decided by the match row itself: clamping
  // the page's end to the base instead would paint a page the focus row is not
  // in, and `splicePage` reports the focus row — find would jump to a blank.
  const aboveBase = focus >= paintedBase;
  const floorRow = aboveBase ? Math.max(lower, paintedBase) : lower;
  const ceilRow = aboveBase ? gap.end : Math.min(gap.end, paintedBase);
  const start = focus - floorRow < BACKFILL_FETCH_ROWS ? floorRow : focus;
  const end = Math.min(ceilRow, start + BACKFILL_FETCH_ROWS);
  return start < end ? { focus, start, end } : null;
}
