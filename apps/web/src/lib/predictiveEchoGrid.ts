// The grid-reading half of predictive local echo: the prediction record, what
// is painted at a cell, whether a prediction agrees with it, and whether a
// column may be covered by an erase. All pure over CellGridFrame/CellSpan —
// predictiveEcho.ts owns every piece of mutable state and calls these.

import {
  CELL_REVERSE,
  columnSpan,
  columnText,
  DEFAULT_COLOR,
  spanIsAtomic,
  type CellGridFrame,
  type CellSpan,
} from "@roost/shared/cell";

export interface Pred {
  row: number;
  col: number;
  /** The predicted glyph, or "" for an ERASE (backspace over a plain cell). */
  ch: string;
  /** Authoritative text at (row,col) when the guess was made. A match that
   *  reproduces it proves nothing about our own echo (mosh CorrectNoCredit). */
  originalCh: string;
  epoch: number;          // tentative_until_epoch
  bornMs: number;         // for RTT sampling + glitch
  inputSeq: bigint;       // admission sequence of the batch that carried this byte
  ackedMs: number | null; // when the worker confirmed the PTY write; null = unproven
}

/** Text painted at viewport (row,col) in a frame's run-length spans.
 *  `null` means a sparse delta did not include the row, while `""` means the
 *  represented row is blank or ends before the requested column. */
export function cellCharAt(frame: CellGridFrame, row: number, col: number): string | null {
  const viewportRow = frame.viewportRows.find((candidate) => candidate.index === row);
  if (!viewportRow) return null;
  return columnText(viewportRow.spans, col);
}

/** An erase prediction is satisfied by either representation of an empty cell:
 *  a right-trimmed row reports "", a row with text to its right reports " ". */
export function predictionMatches(pred: Pred, actual: string): boolean {
  return pred.ch === "" ? actual === "" || actual === " " : actual === pred.ch;
}

/** The application's own echo latency after the PTY write. A contradiction
 *  inside this window is not yet evidence of a wrong guess. */
const ECHO_GRACE_MS = 50;

/** What one frame proves about one prediction.
 *  - `credit`  retire it AND unlock its epoch: proof our echo landed.
 *  - `retire`  drop it having proven nothing.
 *  - `unproven` the frame cannot hold the echo yet (no ack, or a sparse delta
 *    that omits the row); a long-pending one still trips the glitch force-show.
 *  - `echoing` contradicted, but inside the application's echo latency.
 *  - `contradicted` the guess was wrong. */
export type PredictionVerdict =
  | "credit"
  | "retire"
  | "unproven"
  | "echoing"
  | "contradicted";

/** `frameAtMs` is when the frame ARRIVED: the ack and grace comparisons are
 *  meaningless against any other clock. `actual` is null when a sparse delta
 *  omitted the prediction's row. */
export function judgePrediction(
  pred: Pred, actual: string | null, frameAtMs: number,
): PredictionVerdict {
  if (actual !== null && predictionMatches(pred, actual)) {
    // A GLYPH that replaced DIFFERENT text is proof our echo landed, so it
    // credits even before the write ack — waiting for the ack costs the first
    // chars of every burst a whole extra round trip of invisibility. A glyph
    // reproducing the cell's own prior text proves nothing (mosh's
    // CorrectNoCredit), and neither does an ERASE: an untouched cell reads
    // blank too, so it would unlock the gate on no evidence at all.
    return pred.ch !== "" && pred.originalCh !== pred.ch ? "credit" : "retire";
  }
  // Judging an unproven prediction as contradicted is what wiped whole bursts
  // mid-typing: the worker is not known to have written this byte before it
  // produced this frame, so the frame cannot carry the echo.
  if (pred.ackedMs === null || frameAtMs < pred.ackedMs || actual === null) return "unproven";
  return frameAtMs - pred.ackedMs < ECHO_GRACE_MS ? "echoing" : "contradicted";
}

/** An erase cell paints the default terminal background over one column, so it
 *  may only cover a plain narrow cell on an unstyled background. "blank" means
 *  nothing is painted there and no erase is needed at all. */
export function erasableCell(
  spans: readonly CellSpan[], col: number,
): "blank" | "erase" | "refuse" {
  const cell = columnSpan(spans, col);
  if (cell === null) return "blank";
  if (spanIsAtomic(cell.span)) return "refuse";            // wide glyph / grapheme
  if (cell.span.bg !== DEFAULT_COLOR || cell.span.bgRgb !== undefined) return "refuse";
  if ((cell.span.flags & CELL_REVERSE) !== 0) return "refuse";
  return "erase";
}
