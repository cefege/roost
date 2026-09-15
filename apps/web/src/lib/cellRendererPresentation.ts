import {
  spansText,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";

const PAINT_PRESENTATION_ROW_LIMIT = 512;
export const MAX_HELD_SCROLLBACK_ROWS = 2000;

/** Immutable grid identity and absolute range used to validate one history page. */
export interface BackfillAnchor {
  sbBase: number;
  cols: number;
  total: number;
  gridEpoch: string;
}

export interface RendererEpochSeq {
  grid_epoch: string | null;
  seq: number | null;
}

export type ReaderIntent = "live" | "reading";
export type ReaderIntentReason =
  | "native_scroll"
  | "wheel"
  | "touch"
  | "selection"
  | "find";

/** A park whose whole state is a scroll POSITION: re-pinning it to a new
 *  bottom loses nothing. `selection` and `find` own an anchor instead. */
export function isPositionOnlyReaderReason(
  reason: ReaderIntentReason | null,
): boolean {
  return reason === "native_scroll" || reason === "wheel" || reason === "touch";
}

export const RENDERER_HOLD_SELECTION = 1;
export const RENDERER_HOLD_LINK = 2;

export interface LiveInteractionResult {
  reconciled: boolean;
  anchorChanged: boolean;
}

export const NO_LIVE_INTERACTION_RESULT: LiveInteractionResult =
  Object.freeze({ reconciled: false, anchorChanged: false });

export type ReconcileBlockReason =
  | "reader_pending_frame"
  | "selection_hold"
  | "link_hold"
  | "selection_and_link_hold"
  | "predicted_cursor"
  | "pending_render"
  | "not_reconciled"
  | null;

export interface RendererTerminalModeSnapshot {
  alt_screen: boolean;
  cursor_keys_app: boolean;
  bracketed_paste: boolean;
}

export interface ReaderAnchor {
  row: number;
  offsetPx: number;
}

export interface RendererPaintPresentation {
  rows: Array<{ index: number; text: string }>;
  headSpacerPx: number;
  tailGapPx: number;
  readerAnchor: ReaderAnchor | null;
}

export interface RendererPresentationSnapshot {
  captured_at_ms: number;
  canonical: RendererEpochSeq;
  reconciled: RendererEpochSeq;
  reader_intent: ReaderIntent;
  reader_reason: ReaderIntentReason | null;
  hold_mask: { selection: boolean; link: boolean };
  rows: { canonical: number | null; dom: number };
  mode: {
    canonical: RendererTerminalModeSnapshot | null;
    reconciled: RendererTerminalModeSnapshot | null;
  };
  cursor: {
    canonical: { visible: boolean; row: number; column: number } | null;
    dom: {
      visible: boolean | null;
      row: number | null;
      column: number | null;
      connected: boolean;
    };
  };
  cols: { canonical: number | null; dom: number | null };
  at_bottom: boolean;
}

export function createRendererPaintPresentation(
  projection: RendererProjection,
  rowLimitOverride?: number,
): RendererPaintPresentation {
  const painted = projection.paintedHistory;
  const rowLimit = rowLimitOverride ?? PAINT_PRESENTATION_ROW_LIMIT;
  let start = Math.max(0, painted.length - rowLimit);
  const anchor = projection.readerAnchor;
  if (anchor && painted.length > rowLimit) {
    let at = 0;
    while (at < painted.length && painted[at]!.index < anchor.row) at++;
    start = Math.max(0, Math.min(at - (rowLimit >>> 1), painted.length - rowLimit));
  }
  return {
    rows: painted
      .slice(start, start + rowLimit)
      .map((row) => ({ index: row.index, text: spansText(row.spans) })),
    headSpacerPx: parseFloat(projection.paintedSpacerHeight) || 0,
    tailGapPx: projection.gapRows * (
      projection.rowHeight > 0 ? projection.rowHeight : projection.defaultRowHeight
    ),
    readerAnchor: anchor ? { ...anchor } : null,
  };
}

/** Read-only view of renderer-internal state. Both the presentation snapshot
 *  and the incident DOM reader consume it, so the renderer exposes exactly one
 *  accessor and neither consumer can reach a mutable member. */
export interface RendererProjection {
  container: HTMLElement;
  scrollbackEl: HTMLElement;
  viewportEl: HTMLElement;
  canonical: CellGridFrame | null;
  /** Frame the renderer has accepted. At a pre-destructive boundary this is
   *  already the INCOMING canonical, never the committed painted model. */
  applied: CellGridFrame | null;
  canonicalWatermark: RendererEpochSeq;
  reconciledWatermark: RendererEpochSeq;
  readerIntent: ReaderIntent;
  readerReason: ReaderIntentReason | null;
  readerAnchor: ReaderAnchor | null;
  holdMask: number;
  domRows: number;
  reconciledAltScreen: boolean | null;
  reconciledCursorKeysApp: boolean | null;
  reconciledBracketedPaste: boolean | null;
  paintedCursorVisible: boolean | null;
  paintedCursorRow: number;
  paintedCursorCol: number;
  cursorConnected: boolean;
  paintedCols: number | null;
  atBottom: boolean;
  paintedHistory: readonly CellRow[];
  paintedSbBase: number;
  scrollbackLayoutEnd: number;
  /** Reserved height of the unpainted history head, exactly as painted. */
  paintedSpacerHeight: string;
  gapRows: number;
  defaultRowHeight: number;
  rowHeight: number;
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export type RendererIncidentPhase =
  | "pre_apply"
  | "pre_destructive"
  | "pre_history_insert"
  | "post_reconcile";

/** Installed only by an armed terminal incident recorder. `armed` is read at
 *  every renderer call site BEFORE an argument is constructed, so an unarmed
 *  terminal reads no DOM and allocates nothing. */
export interface RendererIncidentObserver {
  readonly armed: boolean;
  observe(phase: RendererIncidentPhase, mode: "full" | "delta" | null): void;
}

/** Two painted history rows are the same row when they paint the same cells;
 *  authoritative history that disagrees with a painted row forces a repair. */
export function sameScrollbackRow(left: CellRow, right: CellRow): boolean {
  if (left === right || (left.index === right.index && left.spans === right.spans)) return true;
  if (left.index !== right.index || left.spans.length !== right.spans.length) return false;
  for (let index = 0; index < left.spans.length; index++) {
    const a = left.spans[index]!;
    const b = right.spans[index]!;
    if (
      a.text !== b.text || a.columns !== b.columns || a.fg !== b.fg || a.bg !== b.bg
      || a.flags !== b.flags || a.fgRgb !== b.fgRgb || a.bgRgb !== b.bgRgb
      || a.linkUri !== b.linkUri || a.linkKey !== b.linkKey
    ) return false;
  }
  return true;
}

export interface RendererReconcileState {
  readerPending: boolean;
  holdMask: number;
  predictedCol: number | null;
  cursorCol: number | null;
  pendingRender: boolean;
  canonical: RendererEpochSeq;
  reconciled: RendererEpochSeq;
}

export function rendererReconcileBlockReason(
  state: RendererReconcileState,
): ReconcileBlockReason {
  if (state.readerPending) return "reader_pending_frame";
  const selection = (state.holdMask & RENDERER_HOLD_SELECTION) !== 0;
  const link = (state.holdMask & RENDERER_HOLD_LINK) !== 0;
  if (selection && link) return "selection_and_link_hold";
  if (selection) return "selection_hold";
  if (link) return "link_hold";
  if (
    state.cursorCol !== null
    && state.predictedCol !== null
    && state.predictedCol !== state.cursorCol
  ) return "predicted_cursor";
  if (state.pendingRender) return "pending_render";
  if (
    state.canonical.grid_epoch !== state.reconciled.grid_epoch
    || state.canonical.seq !== state.reconciled.seq
  ) return "not_reconciled";
  return null;
}

/** Absolute history rows the scroll box shows right now, or null when the
 *  box sits past the painted history. */
export function visibleHistoryRowRange(input: {
  scrollTop: number;
  spacerTop: number;
  clientHeight: number;
  rowHeight: number;
  total: number;
}): { start: number; end: number } | null {
  if (input.rowHeight <= 0 || input.clientHeight <= 0) return null;
  const start = Math.max(
    0,
    Math.floor((input.scrollTop - input.spacerTop) / input.rowHeight),
  );
  const end = Math.min(
    input.total,
    Math.ceil(
      (input.scrollTop + input.clientHeight - input.spacerTop) / input.rowHeight,
    ),
  );
  return start >= end ? null : { start, end };
}

/** Rows that left `previous`'s viewport and became history in `frame`, in
 *  absolute coordinates, or null when the checkpoint promotes nothing. */
export function transitionedViewportRows(
  previous: CellGridFrame | null,
  frame: CellGridFrame | null,
): CellRow[] | null {
  if (
    !frame
    || !previous
    || !frame.full
    || previous.gridEpoch !== frame.gridEpoch
    || previous.cols !== frame.cols
    || previous.rows !== frame.rows
    || previous.altScreen !== frame.altScreen
    || previous.scrollbackTotal >= frame.scrollbackTotal
  ) return null;
  const transitioned = Math.min(
    previous.rows,
    frame.scrollbackTotal - previous.scrollbackTotal,
  );
  if (transitioned === 0) return null;
  return previous.viewportRows.slice(0, transitioned).map((row) => ({
    index: previous.scrollbackTotal + row.index,
    spans: row.spans,
  }));
}

export function createRendererPresentationSnapshot(
  state: RendererProjection,
): RendererPresentationSnapshot {
  const canonical = state.canonical;
  const reconciledMode = state.reconciledAltScreen === null
    || state.reconciledCursorKeysApp === null
    || state.reconciledBracketedPaste === null
    ? null
    : {
      alt_screen: state.reconciledAltScreen,
      cursor_keys_app: state.reconciledCursorKeysApp,
      bracketed_paste: state.reconciledBracketedPaste,
    };
  return {
    captured_at_ms: Date.now(),
    canonical: state.canonicalWatermark,
    reconciled: state.reconciledWatermark,
    reader_intent: state.readerIntent,
    reader_reason: state.readerReason,
    hold_mask: {
      selection: (state.holdMask & RENDERER_HOLD_SELECTION) !== 0,
      link: (state.holdMask & RENDERER_HOLD_LINK) !== 0,
    },
    rows: { canonical: canonical?.rows ?? null, dom: state.domRows },
    mode: {
      canonical: canonical ? {
        alt_screen: canonical.altScreen,
        cursor_keys_app: canonical.cursorKeysApp,
        bracketed_paste: canonical.bracketedPaste,
      } : null,
      reconciled: reconciledMode,
    },
    cursor: {
      canonical: canonical ? {
        visible: canonical.cursorVisible,
        row: canonical.cursorRow,
        column: canonical.cursorCol,
      } : null,
      dom: {
        visible: state.paintedCursorVisible,
        row: state.paintedCursorVisible === true && state.paintedCursorRow >= 0
          ? state.paintedCursorRow
          : null,
        column: state.paintedCursorVisible === true && state.paintedCursorCol >= 0
          ? state.paintedCursorCol
          : null,
        connected: state.cursorConnected,
      },
    },
    cols: { canonical: canonical?.cols ?? null, dom: state.paintedCols },
    at_bottom: state.atBottom,
  };
}

export function readerAnchorAtScroll(
  scrollTop: number,
  spacerTop: number,
  rowHeight: number,
  layoutEnd: number,
): ReaderAnchor | null {
  const exact = (scrollTop - spacerTop) / rowHeight;
  if (exact >= layoutEnd) return null;
  const row = Math.max(0, Math.floor(exact));
  return {
    row,
    offsetPx: Math.max(0, Math.min(
      rowHeight,
      scrollTop - spacerTop - row * rowHeight,
    )),
  };
}
