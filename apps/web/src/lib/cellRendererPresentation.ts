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

export function createRendererPaintPresentation(opts: {
  paintedRows: readonly CellRow[];
  readerAnchor: ReaderAnchor | null;
  paintedSpacerHeight: string;
  gapRows: number;
  rowHeight: number;
  defaultRowHeight: number;
  rowLimit?: number;
}): RendererPaintPresentation {
  const rowLimit = opts.rowLimit ?? PAINT_PRESENTATION_ROW_LIMIT;
  let start = Math.max(0, opts.paintedRows.length - rowLimit);
  const anchor = opts.readerAnchor;
  if (anchor && opts.paintedRows.length > rowLimit) {
    let at = 0;
    while (at < opts.paintedRows.length && opts.paintedRows[at]!.index < anchor.row) at++;
    start = Math.max(0, Math.min(
      at - (rowLimit >>> 1),
      opts.paintedRows.length - rowLimit,
    ));
  }
  return {
    rows: opts.paintedRows
      .slice(start, start + rowLimit)
      .map((row) => ({ index: row.index, text: spansText(row.spans) })),
    headSpacerPx: parseFloat(opts.paintedSpacerHeight) || 0,
    tailGapPx: opts.gapRows * (
      opts.rowHeight > 0 ? opts.rowHeight : opts.defaultRowHeight
    ),
    readerAnchor: anchor ? { ...anchor } : null,
  };
}

export interface RendererPresentationState {
  canonical: CellGridFrame | null;
  canonicalWatermark: RendererEpochSeq;
  reconciledWatermark: RendererEpochSeq;
  readerIntent: ReaderIntent;
  readerReason: ReaderIntentReason | null;
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
}

export function createRendererPresentationSnapshot(
  state: RendererPresentationState,
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
