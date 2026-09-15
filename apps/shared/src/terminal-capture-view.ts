// Exact canonical-view comparison for terminal incident capture. One
// implementation of "are these two terminal states the same state", used by the
// worker (fresh core vs emitted-frame fold), the coordinator replica, the
// browser replica and scripts/replay-terminal-incident.ts.
// Delivery representation (full/baseSeq, history pages, append payloads) is
// excluded on purpose: a sparse delta and a full frame that mean the same
// screen must compare equal, and a digest would prove neither way.

import {
  rowColumns,
  spansText,
  type CellGridFrame,
  type CellRow,
  type CellSpan,
  type MouseTracking,
} from "./cell/types.ts";

/** Viewport-only projection of a terminal state, independent of how it shipped. */
export interface TerminalCanonicalView {
  readonly cols: number;
  readonly rows: number;
  readonly cursorRow: number;
  readonly cursorCol: number;
  readonly cursorVisible: boolean;
  readonly altScreen: boolean;
  readonly cursorKeysApp: boolean;
  readonly bracketedPaste: boolean;
  readonly mouseTracking: MouseTracking;
  readonly mouseSgr: boolean;
  readonly focusEvents: boolean;
  readonly sbBase: number;
  readonly scrollbackTotal: number;
  /** Dense, index-ordered viewport rows. */
  readonly viewportRows: readonly CellRow[];
}

export type TerminalCanonicalField =
  | "cols" | "rows"
  | "cursorRow" | "cursorCol" | "cursorVisible"
  | "altScreen" | "cursorKeysApp" | "bracketedPaste"
  | "mouseTracking" | "mouseSgr" | "focusEvents"
  | "sbBase" | "scrollbackTotal"
  | "viewportRowCount" | "rowIndex";

export type TerminalCellField =
  | "text" | "columns" | "fg" | "bg" | "flags" | "fgRgb" | "bgRgb"
  | "linkUri" | "linkKey" | "rowColumns";

/** First exact disagreement between two canonical views. `row`/`column` are
 *  present only for a cell-level difference so a report can name the painted
 *  coordinate without carrying terminal text. */
export type TerminalCanonicalDifference =
  | {
      readonly kind: "state";
      readonly field: TerminalCanonicalField;
      readonly left: string;
      readonly right: string;
    }
  | {
      readonly kind: "row";
      readonly row: number;
      readonly column: number;
      readonly field: TerminalCellField;
      readonly left: string;
      readonly right: string;
    };

/** Project a wire frame. Only a dense full viewport is a canonical view; a
 *  sparse delta must be folded onto its baseline before comparison, which is
 *  what `null` here forces a caller to notice instead of silently comparing a
 *  patch against a screen.
 *
 *  The row array is COPIED. `applyDelta` consumes and mutates the frame it
 *  folds onto, so a view that aliased `frame.viewportRows` would be rewritten
 *  by every later delta — a checkpoint retained across a fold would silently
 *  report the newest screen under an older sequence, inventing a divergence at
 *  exactly the row an application redrew. Spans are immutable and shared, and
 *  a viewport is at most a few hundred rows, so the copy is shallow and cheap.
 *  Nothing in a type can express "do not retain this", hence the copy. */
export function canonicalViewOfFrame(frame: CellGridFrame): TerminalCanonicalView | null {
  if (frame.viewportRows.length !== frame.rows) return null;
  for (let idx = 0; idx < frame.viewportRows.length; idx++) {
    if (frame.viewportRows[idx]!.index !== idx) return null;
  }
  return {
    cols: frame.cols,
    rows: frame.rows,
    cursorRow: frame.cursorRow,
    cursorCol: frame.cursorCol,
    cursorVisible: frame.cursorVisible,
    altScreen: frame.altScreen,
    cursorKeysApp: frame.cursorKeysApp,
    bracketedPaste: frame.bracketedPaste,
    mouseTracking: frame.mouseTracking,
    mouseSgr: frame.mouseSgr,
    focusEvents: frame.focusEvents,
    sbBase: frame.sbBase,
    scrollbackTotal: frame.scrollbackTotal,
    viewportRows: frame.viewportRows.slice(),
  };
}

const STATE_FIELDS: readonly TerminalCanonicalField[] = [
  "cols", "rows", "cursorRow", "cursorCol", "cursorVisible", "altScreen",
  "cursorKeysApp", "bracketedPaste", "mouseTracking", "mouseSgr",
  "focusEvents", "sbBase", "scrollbackTotal",
];

/** Exact comparison. Returns the FIRST difference in a stable order (state
 *  fields, then row count, then rows top-to-bottom and left-to-right), or null
 *  when the two views are the same terminal state. */
export function compareCanonicalViews(
  left: TerminalCanonicalView,
  right: TerminalCanonicalView,
): TerminalCanonicalDifference | null {
  for (const field of STATE_FIELDS) {
    const leftValue = left[field as keyof TerminalCanonicalView];
    const rightValue = right[field as keyof TerminalCanonicalView];
    if (leftValue !== rightValue) {
      return { kind: "state", field, left: String(leftValue), right: String(rightValue) };
    }
  }
  if (left.viewportRows.length !== right.viewportRows.length) {
    return {
      kind: "state",
      field: "viewportRowCount",
      left: String(left.viewportRows.length),
      right: String(right.viewportRows.length),
    };
  }
  for (let idx = 0; idx < left.viewportRows.length; idx++) {
    const leftRow = left.viewportRows[idx]!;
    const rightRow = right.viewportRows[idx]!;
    if (leftRow.index !== rightRow.index) {
      return {
        kind: "state",
        field: "rowIndex",
        left: String(leftRow.index),
        right: String(rightRow.index),
      };
    }
    const difference = compareRows(idx, leftRow.spans, rightRow.spans);
    if (difference) return difference;
  }
  return null;
}

/** Column-aligned span walk: two rows carrying the same painted cells over a
 *  different span split are equal, so a re-coalesced fold cannot masquerade as
 *  corruption. Text is compared per column, never as whole-row strings. */
function compareRows(
  row: number,
  left: readonly CellSpan[],
  right: readonly CellSpan[],
): TerminalCanonicalDifference | null {
  const leftColumns = rowColumns(left);
  const rightColumns = rowColumns(right);
  if (leftColumns !== rightColumns) {
    return {
      kind: "row",
      row,
      column: Math.min(leftColumns, rightColumns),
      field: "rowColumns",
      left: String(leftColumns),
      right: String(rightColumns),
    };
  }
  const leftCells = expandRow(left);
  const rightCells = expandRow(right);
  for (let column = 0; column < leftCells.length; column++) {
    const leftCell = leftCells[column]!;
    const rightCell = rightCells[column]!;
    const field = firstCellFieldDifference(leftCell, rightCell);
    if (field) {
      return {
        kind: "row",
        row,
        column,
        field,
        left: describeCellField(leftCell, field),
        right: describeCellField(rightCell, field),
      };
    }
  }
  return null;
}

interface ExpandedCell {
  readonly text: string;
  readonly columns: number;
  readonly span: CellSpan;
}

/** One entry per grid column. A wide/atomic span claims its lead column with
 *  its whole glyph and its continuation columns with the empty string, which is
 *  exactly how the painted grid reads. */
function expandRow(spans: readonly CellSpan[]): ExpandedCell[] {
  const cells: ExpandedCell[] = [];
  for (const span of spans) {
    const atomic = span.columns !== span.text.length;
    if (atomic) {
      cells.push({ text: span.text, columns: span.columns, span });
      for (let extra = 1; extra < span.columns; extra++) {
        cells.push({ text: "", columns: 0, span });
      }
      continue;
    }
    for (let idx = 0; idx < span.columns; idx++) {
      cells.push({ text: span.text[idx] ?? "", columns: 1, span });
    }
  }
  return cells;
}

function firstCellFieldDifference(
  left: ExpandedCell,
  right: ExpandedCell,
): TerminalCellField | null {
  if (left.text !== right.text) return "text";
  if (left.columns !== right.columns) return "columns";
  if (left.span.fg !== right.span.fg) return "fg";
  if (left.span.bg !== right.span.bg) return "bg";
  if (left.span.flags !== right.span.flags) return "flags";
  if ((left.span.fgRgb ?? null) !== (right.span.fgRgb ?? null)) return "fgRgb";
  if ((left.span.bgRgb ?? null) !== (right.span.bgRgb ?? null)) return "bgRgb";
  if ((left.span.linkUri ?? null) !== (right.span.linkUri ?? null)) return "linkUri";
  if ((left.span.linkKey ?? null) !== (right.span.linkKey ?? null)) return "linkKey";
  return null;
}

/** Bounded, CONTENT-FREE field description: a difference is quoted into signals
 *  and replay reports, so the two fields that can carry terminal content —
 *  `text` and `linkUri` — report only a length. A codepoint list would be the
 *  characters themselves, reversibly. The owner-only bundle keeps the rows. */
function describeCellField(cell: ExpandedCell, field: TerminalCellField): string {
  switch (field) {
    case "text":
      return `len=${cell.text.length}`;
    case "columns":
      return String(cell.columns);
    case "fg":
      return String(cell.span.fg);
    case "bg":
      return String(cell.span.bg);
    case "flags":
      return `0x${cell.span.flags.toString(16)}`;
    case "fgRgb":
      return cell.span.fgRgb === undefined ? "none" : `0x${cell.span.fgRgb.toString(16)}`;
    case "bgRgb":
      return cell.span.bgRgb === undefined ? "none" : `0x${cell.span.bgRgb.toString(16)}`;
    case "linkUri":
      return cell.span.linkUri === undefined ? "none" : `len=${cell.span.linkUri.length}`;
    case "linkKey":
      return cell.span.linkKey ?? "none";
    case "rowColumns":
      return String(cell.columns);
  }
}

/** PAINTED-TEXT identity of one row. Deliberately NOT web's `rowHash`, which
 *  folds in the span SPLIT so the viewport diff can repaint a re-coalesced row
 *  — here a model row and the DOM row it painted must compare equal across any
 *  legal re-split, and the value must be safe to put in a report, so it is a
 *  fingerprint rather than the characters. */
export function paintedRowFingerprint(spans: readonly CellSpan[]): number {
  return paintedTextFingerprint(spansText(spans), rowColumns(spans));
}

/** Same identity for a string already read out of the DOM. */
export function paintedTextFingerprint(text: string, columns: number): number {
  let hash = Math.imul(2_166_136_261 ^ columns, 16_777_619);
  hash = Math.imul(hash ^ text.length, 16_777_619);
  for (let idx = 0; idx < text.length; idx++) {
    hash = Math.imul(hash ^ text.charCodeAt(idx), 16_777_619);
  }
  return hash >>> 0;
}
