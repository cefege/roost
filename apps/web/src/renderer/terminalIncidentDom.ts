// Reads the ACTUAL painted terminal DOM for one armed incident recorder and
// compares it with the renderer's committed painted model. Nothing here mutates
// the renderer, the DOM, the selection or the scroll position.
// Called by terminalIncidentCapture.ts through CellGridRenderer's guarded
// observer hooks; depends on cellRendererPresentation.ts, cellRow.ts and
// @roost/protocol/terminal-capture.

import {
  TERMINAL_CAPTURE_LIMITS,
  paintedRowFingerprint,
  paintedTextFingerprint,
  type TerminalBrowserApplyMode,
  type TerminalBrowserPaintedState,
  type TerminalBrowserPhase,
  type TerminalCaptureHistoryRange,
  type TerminalCaptureOmission,
  type TerminalCaptureReason,
  type TerminalCaptureStreamIdentity,
  type TerminalDomRow,
} from "@roost/protocol/terminal-capture";
import type { CellGridFrame, CellRow } from "@roost/protocol/cell";
import { ROW_COLUMNS_ATTR } from "./cellRow.ts";
import type { RendererProjection } from "./cellRendererPresentation.ts";

/** Painted state the renderer had COMMITTED at its last completed
 *  reconciliation, as absolute history index → painted identity over the
 *  retained tail. The pre-destructive checker compares the DOM with this and
 *  never with the incoming canonical frame, which has already replaced it. */
export interface CommittedPaintedModel {
  readonly atMs: number;
  readonly gridEpoch: string;
  readonly seq: number;
  readonly cols: number;
  readonly rows: number;
  readonly altScreen: boolean;
  readonly history: ReadonlyMap<number, number>;
  readonly historyStart: number;
  readonly historyEnd: number;
  readonly viewport: readonly number[];
}

export type PaintedConflictKind =
  | "history_duplicate_index"
  | "history_out_of_order"
  | "history_model_mismatch"
  | "viewport_row_count"
  | "viewport_text_mismatch";

/** `index` is the absolute history index or viewport row it failed at. */
export interface PaintedConflict {
  readonly kind: PaintedConflictKind;
  readonly reason: TerminalCaptureReason;
  readonly index: number | null;
  readonly occurrences: number;
}

export interface PaintedStateInput {
  readonly projection: RendererProjection;
  readonly phase: TerminalBrowserPhase;
  readonly applyMode: TerminalBrowserApplyMode | null;
  /** Replica canonical viewport, read independently of the renderer. */
  readonly canonical: CellGridFrame | null;
  readonly committed: TerminalCaptureStreamIdentity | null;
  readonly active: boolean;
  readonly visible: boolean;
}

export function streamIdentityOfFrame(
  frame: CellGridFrame | null,
): TerminalCaptureStreamIdentity | null {
  if (!frame) return null;
  return {
    stream_id: frame.streamId,
    grid_epoch: frame.gridEpoch,
    seq: String(frame.seq),
    base_seq: frame.full ? null : String(frame.baseSeq),
    cols: frame.cols,
    rows: frame.rows,
  };
}

/** Bounded fingerprint model of what the renderer just committed. Only the
 *  history tail is retained, so a 2,000-row painted history never costs a
 *  full scan per reconciliation. */
export function committedPaintedModel(
  projection: RendererProjection,
  frame: CellGridFrame,
  reuse: CommittedPaintedModel | null = null,
): CommittedPaintedModel {
  const painted = projection.paintedHistory;
  const start = Math.max(0, painted.length - TERMINAL_CAPTURE_LIMITS.browserHistoryTailRows);
  const historyStart = painted[start]?.index ?? Number.MAX_SAFE_INTEGER;
  const historyEnd = painted[painted.length - 1]?.index ?? -1;
  // Painted history rows are immutable and append-only inside one epoch, so an
  // unchanged (first, last, count) triple is the same tail: re-fingerprinting
  // 128 rows on every applied frame would buy nothing.
  const reusable = reuse !== null
    && reuse.historyStart === historyStart
    && reuse.historyEnd === historyEnd
    && reuse.history.size === painted.length - start;
  let history = reusable && reuse !== null ? reuse.history : null;
  if (history === null) {
    const rebuilt = new Map<number, number>();
    for (let at = start; at < painted.length; at++) {
      const row = painted[at]!;
      rebuilt.set(row.index, paintedRowFingerprint(row.spans));
    }
    history = rebuilt;
  }
  const viewport: number[] = [];
  for (const row of frame.viewportRows) viewport.push(paintedRowFingerprint(row.spans));
  return {
    atMs: Date.now(),
    gridEpoch: frame.gridEpoch,
    seq: frame.seq,
    cols: frame.cols,
    rows: frame.rows,
    altScreen: frame.altScreen,
    history,
    historyStart,
    historyEnd,
    viewport,
  };
}

/** Strict structural invariants only. `viewport` is checked only when the
 *  caller has already proven the DOM is not reader-held and the committed
 *  model names the same geometry, epoch and sequence. */
export function findPaintedConflict(
  projection: RendererProjection,
  committed: CommittedPaintedModel | null,
  checkViewport: boolean,
): PaintedConflict | null {
  const nodes = readDomHistoryNodes(projection);
  const structural = firstHistoryOrderConflict(nodes);
  if (structural) return structural;
  if (committed) {
    const tail = domHistoryTail(nodes, TERMINAL_CAPTURE_LIMITS.browserHistoryTailRows);
    const model = firstHistoryModelConflict(tail, committed);
    if (model) return model;
    if (checkViewport) {
      return firstViewportConflict(readDomViewportRows(projection).rows, committed);
    }
  }
  return null;
}

/** One bounded snapshot of what is actually painted. DOM text is read from the
 *  nodes, never reconstructed from the renderer's frame: a model projection
 *  cannot prove what the reader is looking at. */
export function readPaintedState(input: PaintedStateInput): TerminalBrowserPaintedState {
  const projection = input.projection;
  const omissions: TerminalCaptureOmission[] = [];
  const viewport = readDomViewportRows(projection);
  if (viewport.dropped > 0) {
    omissions.push(rowOmission("dom_viewport", viewport.dropped, null, null));
  }
  const historyBudget = Math.max(
    0,
    Math.min(
      TERMINAL_CAPTURE_LIMITS.browserHistoryTailRows,
      TERMINAL_CAPTURE_LIMITS.browserRowsMax - viewport.rows.length,
    ),
  );
  const nodes = readDomHistoryNodes(projection);
  const keptHistory = domHistoryTail(nodes, historyBudget);
  if (keptHistory.length < nodes.length) {
    omissions.push(rowOmission(
      "dom_history",
      nodes.length - keptHistory.length,
      absoluteRowIndex(nodes[0]!),
      keptHistory[0]?.index ?? null,
    ));
  }
  const painted = projection.paintedHistory;
  const modelStart = Math.max(0, painted.length - historyBudget);
  if (modelStart > 0) {
    omissions.push(rowOmission(
      "painted_model_history",
      modelStart,
      painted[0]?.index ?? null,
      painted[modelStart]?.index ?? null,
    ));
  }
  return {
    at_ms: Date.now(),
    phase: input.phase,
    apply_mode: input.applyMode,
    canonical: input.canonical,
    committed: input.committed,
    pending: streamIdentityOfFrame(projection.canonical),
    painted_model_history: painted.slice(modelStart) as readonly CellRow[],
    dom_history: keptHistory,
    dom_viewport: viewport.rows,
    gaps: readHistoryGaps(projection),
    cursor: {
      row: projection.paintedCursorRow,
      col: projection.paintedCursorCol,
      visible: projection.paintedCursorVisible === true,
    },
    scroll: {
      top: projection.scrollTop,
      height: projection.scrollHeight,
      client_height: projection.clientHeight,
      row_height: projection.rowHeight,
    },
    reader: {
      intent: projection.readerIntent,
      reason: projection.readerReason,
      hold_mask: projection.holdMask,
    },
    active: input.active,
    visible: input.visible,
    omissions,
  };
}

/** Painted history row ELEMENTS, oldest first. References only: the order
 *  invariant is checked on every reconciliation, so it must not build one
 *  string per painted row. */
function readDomHistoryNodes(projection: RendererProjection): HTMLElement[] {
  const nodes: HTMLElement[] = [];
  for (const block of projection.scrollbackEl.children) {
    if ((block as HTMLElement).className !== "cell-block") continue;
    for (const node of block.children) {
      const el = node as HTMLElement;
      if (el.className === "cell-row") nodes.push(el);
    }
  }
  return nodes;
}

/** Painted text and identity for the last `limit` history rows — the only
 *  rows a model comparison or an export actually reads. */
function domHistoryTail(nodes: readonly HTMLElement[], limit: number): TerminalDomRow[] {
  const rows: TerminalDomRow[] = [];
  for (let at = Math.max(0, nodes.length - limit); at < nodes.length; at++) {
    rows.push(domRow(nodes[at]!, at, absoluteRowIndex(nodes[at]!)));
  }
  return rows;
}

/** Live grid rows, in paint order, with the count a taller-than-budget grid
 *  could not carry. Visible rows are never dropped to fit history. */
interface DomViewportRead {
  readonly rows: TerminalDomRow[];
  readonly dropped: number;
}

function readDomViewportRows(projection: RendererProjection): DomViewportRead {
  const rows: TerminalDomRow[] = [];
  let dropped = 0;
  for (const node of projection.viewportEl.children) {
    const el = node as HTMLElement;
    if (el.className !== "cell-row") continue;
    if (rows.length >= TERMINAL_CAPTURE_LIMITS.browserRowsMax) dropped++;
    else rows.push(domRow(el, rows.length, null));
  }
  return { rows, dropped };
}

function readHistoryGaps(projection: RendererProjection): TerminalCaptureHistoryRange[] {
  const gaps: TerminalCaptureHistoryRange[] = [];
  const evicted = projection.paintedSbBase;
  if (evicted > 0) {
    gaps.push({ start: "0", end: String(evicted), status: "evicted", rows: evicted });
  }
  for (const node of projection.scrollbackEl.children) {
    const el = node as HTMLElement;
    if (el.className !== "cell-sb-gap") continue;
    const start = Number(el.dataset.startRow);
    const end = Number(el.dataset.endRow);
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start >= end) continue;
    gaps.push({
      start: String(start),
      end: String(end),
      status: "unavailable",
      rows: end - start,
    });
  }
  return gaps;
}

function domRow(el: HTMLElement, order: number, index: number | null): TerminalDomRow {
  const text = el.textContent ?? "";
  const columns = Number(el.getAttribute(ROW_COLUMNS_ATTR) ?? text.length);
  const exact = Number.isSafeInteger(columns) ? columns : text.length;
  return {
    order,
    index,
    columns: exact,
    fingerprint: paintedIdentity(text, exact),
    text,
    span_count: el.children.length,
  };
}

/** A row with zero grid columns paints one blank to keep its line box, so its
 *  painted identity is the empty string — not that padding space. */
function paintedIdentity(text: string, columns: number): number {
  return paintedTextFingerprint(columns === 0 ? "" : text, columns);
}

function absoluteRowIndex(el: HTMLElement): number | null {
  const raw = el.dataset.rowIndex;
  if (raw === undefined) return null;
  const index = Number(raw);
  return Number.isSafeInteger(index) ? index : null;
}

/** Painted history indices must be strictly increasing. A duplicated or
 *  re-ordered absolute index is the duplicated-tail corruption class; equal
 *  TEXT at two distinct indices is legitimate and stays silent. */
function firstHistoryOrderConflict(nodes: readonly HTMLElement[]): PaintedConflict | null {
  let previous: number | null = null;
  for (const node of nodes) {
    const index = absoluteRowIndex(node);
    if (index === null) {
      return { kind: "history_out_of_order", reason: "history_identity", index: null, occurrences: 1 };
    }
    if (previous !== null && index <= previous) {
      let occurrences = 0;
      for (const other of nodes) if (absoluteRowIndex(other) === index) occurrences++;
      return {
        kind: index === previous ? "history_duplicate_index" : "history_out_of_order",
        reason: "history_identity",
        index,
        occurrences,
      };
    }
    previous = index;
  }
  return null;
}

/** Every DOM row inside the committed tail must be that row, and the tail must
 *  hold exactly the rows the model committed. */
function firstHistoryModelConflict(
  rows: readonly TerminalDomRow[],
  committed: CommittedPaintedModel,
): PaintedConflict | null {
  let compared = 0;
  for (const row of rows) {
    if (row.index === null || row.index < committed.historyStart) continue;
    const expected = committed.history.get(row.index);
    if (expected === undefined || expected !== row.fingerprint) {
      return modelMismatch(row.index, 1);
    }
    compared++;
  }
  if (compared === committed.history.size) return null;
  return modelMismatch(committed.historyStart, Math.abs(committed.history.size - compared));
}

function modelMismatch(index: number, occurrences: number): PaintedConflict {
  return {
    kind: "history_model_mismatch",
    reason: "history_identity",
    index,
    occurrences,
  };
}

function firstViewportConflict(
  rows: readonly TerminalDomRow[],
  committed: CommittedPaintedModel,
): PaintedConflict | null {
  const reason = "viewport_model";
  if (rows.length !== committed.viewport.length) {
    const missing = Math.abs(rows.length - committed.viewport.length);
    return { kind: "viewport_row_count", reason, index: null, occurrences: missing };
  }
  for (let at = 0; at < rows.length; at++) {
    if (rows[at]!.fingerprint === committed.viewport[at]!) continue;
    return { kind: "viewport_text_mismatch", reason, index: at, occurrences: 1 };
  }
  return null;
}

function rowOmission(
  name: string,
  dropped: number,
  from: number | null,
  through: number | null,
): TerminalCaptureOmission {
  const range = from === null || through === null
    ? null
    : { start: String(from), end: String(through) };
  return {
    kind: "rows",
    name,
    reason: "evidence_trimmed",
    dropped_count: dropped,
    dropped_bytes: 0,
    range,
  };
}
