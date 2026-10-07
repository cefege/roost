//! The read-only views a terminal pane publishes about itself: the epoch
//! watermarks, the paint presentation a stress harness reads, and the snapshot
//! diagnostics diff against. Plain data with no DOM, so what the browser shows and
//! what a native test asserts are the same values; the smoke API and the incident
//! scanner read them. Ports `apps/web/src/renderer/cellRendererPresentation.ts`.

use std::rc::Rc;

use roost_protocol::cell::{CellGridFrame, CellRow, spans_text};

use crate::block_placeholder::DEFAULT_CELL_ROW_PX;
use crate::reader_intent::{ReaderAnchor, ReaderIntent, ReaderIntentReason};

/// Rows of one presentation by default. Enough to show the reader's region of
/// history without serialising a 2000-row sheet per sample.
const PAINT_PRESENTATION_ROW_LIMIT: usize = 512;

/// The `(grid epoch, sequence)` pair a watermark names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RendererEpochSeq {
    /// The worker's opaque grid-numbering epoch, or `None` before one is
    /// reconciled.
    pub grid_epoch: Option<String>,
    /// The frame sequence this watermark reaches, or `None` before one is.
    pub seq: Option<u64>,
}

/// Immutable grid identity and absolute range used to validate one history page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillAnchor {
    /// The painted HEAD BASE — the splice boundary the DOM can be prepended at
    /// and evicted to. Rows from it up are painted; an eviction moves it to one
    /// past the last row it dropped, so the head gap that eviction leaves can
    /// still collapse against it.
    pub sb_base: u32,
    /// Columns the grid paints, which a backfilled page must match.
    pub cols: u32,
    /// Total retained scrollback lines at frame time.
    pub total: u64,
    /// The epoch the page must be addressed to.
    pub grid_epoch: String,
}

/// What a reader event did to the DOM, so a caller can tell a real resume from
/// a no-op without diffing the presentation itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveInteractionResult {
    /// A canonical frame was reconciled into the DOM during the transition.
    pub reconciled: bool,
    /// The backfill anchor moved, so an in-flight history page is now invalid.
    pub anchor_changed: bool,
}

/// The result of an interaction that changed nothing.
pub const NO_LIVE_INTERACTION_RESULT: LiveInteractionResult = LiveInteractionResult {
    reconciled: false,
    anchor_changed: false,
};

/// One painted row, flattened to the two fields a reader cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedRowText {
    /// Absolute history row index.
    pub index: u32,
    /// The row's text as painted.
    pub text: String,
}

/// What a paint actually shows: the visible history rows plus the two reserved
/// pixel bands above and below them.
#[derive(Debug, Clone, PartialEq)]
pub struct RendererPaintPresentation {
    /// Painted history rows around the reader's anchor, oldest first.
    pub rows: Vec<PaintedRowText>,
    /// Reserved height of the unpainted history head, in pixels.
    pub head_spacer_px: f64,
    /// Reserved height of the unpainted interior/tail gaps, in pixels.
    pub tail_gap_px: f64,
    /// Where the reader's own scroll position sits, when it is inside painted
    /// layout.
    pub reader_anchor: Option<ReaderAnchor>,
}

/// The three terminal modes the front end both receives and must keep painted
/// in step with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererTerminalModeSnapshot {
    pub alt_screen: bool,
    pub cursor_keys_app: bool,
    pub bracketed_paste: bool,
}

/// Every renderer internal a diagnostic consumer may read, and none it may
/// write. There is exactly one such view so the presentation snapshot and the
/// incident DOM reader can never disagree about what the renderer believes.
#[derive(Debug, Clone, PartialEq)]
pub struct RendererProjection {
    /// The newest frame the renderer has accepted, including one held back for
    /// a parked reader. Shared with the renderer by refcount.
    pub canonical: Option<Rc<CellGridFrame>>,
    /// The frame the painted DOM was built from.
    pub applied: Option<Rc<CellGridFrame>>,
    /// How far canonical has advanced.
    pub canonical_watermark: RendererEpochSeq,
    /// How far the DOM has been reconciled.
    pub reconciled_watermark: RendererEpochSeq,
    /// Whether the pane is painting at the tail or frozen for a reader.
    pub reader_intent: ReaderIntent,
    /// Why the reader is parked.
    pub reader_reason: Option<ReaderIntentReason>,
    /// Where the reader's scroll position sits in absolute rows.
    pub reader_anchor: Option<ReaderAnchor>,
    /// The composed hold mask.
    pub hold_mask: u32,
    /// Row elements currently in the viewport.
    pub dom_rows: usize,
    /// The terminal modes the DOM has been reconciled to, or `None` before the
    /// first reconcile.
    pub reconciled_mode: Option<RendererTerminalModeSnapshot>,
    /// The cursor state the DOM carries, which can be a leading prediction.
    pub painted_cursor_visible: Option<bool>,
    pub painted_cursor_row: i64,
    pub painted_cursor_col: i64,
    /// Whether the cursor element is still inside the painted viewport.
    pub cursor_connected: bool,
    /// Columns the container's `--cell-cols` currently says.
    pub painted_cols: Option<u32>,
    /// The exact bottom clamp.
    pub at_bottom: bool,
    /// The follow band, beside the exact clamp.
    pub follows_bottom: bool,
    /// The painted immutable history, ascending.
    pub painted_history: Vec<CellRow>,
    /// The painted head base — the splice boundary, one past the last evicted
    /// row rather than the first still-painted one.
    pub painted_sb_base: u32,
    /// One past the last row the scroll space reserves.
    pub scrollback_layout_end: u64,
    /// Reserved height of the head, exactly as painted.
    pub painted_spacer_height: String,
    /// Rows reserved by interior and tail gaps.
    pub gap_rows: u64,
    /// Measured row height, or zero before the pane has measured one.
    pub row_height: f64,
    /// The row height every derived offset falls back to.
    pub default_row_height: f64,
    /// Live scroll geometry, in client pixels.
    pub scroll_top: f64,
    pub scroll_height: f64,
    pub client_height: f64,
}

/// The shape the diagnostic snapshot and the smoke API publish.
#[derive(Debug, Clone, PartialEq)]
pub struct RendererPresentationSnapshot {
    pub captured_at_ms: f64,
    pub canonical: RendererEpochSeq,
    pub reconciled: RendererEpochSeq,
    pub reader_intent: ReaderIntent,
    pub reader_reason: Option<ReaderIntentReason>,
    pub hold_mask_selection: bool,
    pub hold_mask_link: bool,
    pub canonical_rows: Option<u32>,
    pub dom_rows: usize,
    pub canonical_mode: Option<RendererTerminalModeSnapshot>,
    pub reconciled_mode: Option<RendererTerminalModeSnapshot>,
    pub canonical_cursor: Option<(bool, u32, u32)>,
    pub painted_cursor_visible: Option<bool>,
    pub painted_cursor_row: Option<i64>,
    pub painted_cursor_col: Option<i64>,
    pub cursor_connected: bool,
    pub canonical_cols: Option<u32>,
    pub painted_cols: Option<u32>,
    pub at_bottom: bool,
    pub follows_bottom: bool,
}

/// Build the paint presentation: a window of painted history centred on the
/// reader's anchor, plus the two reserved bands.
pub fn create_renderer_paint_presentation(
    painted: &[CellRow],
    painted_spacer_height: &str,
    gap_rows: u64,
    row_height: f64,
    reader_anchor: Option<ReaderAnchor>,
    row_limit_override: Option<usize>,
) -> RendererPaintPresentation {
    let row_limit = row_limit_override.unwrap_or(PAINT_PRESENTATION_ROW_LIMIT);
    let mut start = painted.len().saturating_sub(row_limit);
    if let Some(anchor) = reader_anchor
        && painted.len() > row_limit
    {
        let at = painted.partition_point(|row| row.index < anchor.row);
        start = at
            .saturating_sub(row_limit / 2)
            .min(painted.len().saturating_sub(row_limit));
    }
    let pitch = if row_height > 0.0 {
        row_height
    } else {
        DEFAULT_CELL_ROW_PX
    };
    RendererPaintPresentation {
        rows: painted
            .iter()
            .skip(start)
            .take(row_limit)
            .map(|row| PaintedRowText {
                index: row.index,
                text: spans_text(&row.spans),
            })
            .collect(),
        head_spacer_px: parse_css_px(painted_spacer_height),
        tail_gap_px: gap_rows as f64 * pitch,
        reader_anchor,
    }
}

/// The pixel count a CSS length string names, unit stripped.
///
/// The spacer is stamped as `"{:.2}px"`, and v2 reads it with `parseFloat`,
/// which stops at the first non-numeric character. A strict whole-string parse
/// reads every reserved height as zero — a diagnostics surface that reports a
/// full head spacer as empty. A value that is not a finite pixel count is zero
/// rather than NaN, which is what v2's `|| 0` gives it.
fn parse_css_px(value: &str) -> f64 {
    value
        .trim()
        .trim_end_matches("px")
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|pixels| pixels.is_finite())
        .unwrap_or(0.0)
}

/// Build the presentation snapshot the smoke API publishes and diagnostics diff.
pub fn create_renderer_presentation_snapshot(
    state: &RendererProjection,
    captured_at_ms: f64,
) -> RendererPresentationSnapshot {
    let canonical = state.canonical.as_ref();
    RendererPresentationSnapshot {
        captured_at_ms,
        canonical: state.canonical_watermark.clone(),
        reconciled: state.reconciled_watermark.clone(),
        reader_intent: state.reader_intent,
        reader_reason: state.reader_reason,
        hold_mask_selection: state.hold_mask & crate::reader_intent::RENDERER_HOLD_SELECTION != 0,
        hold_mask_link: state.hold_mask & crate::reader_intent::RENDERER_HOLD_LINK != 0,
        canonical_rows: canonical.map(|frame| frame.rows),
        dom_rows: state.dom_rows,
        canonical_mode: canonical.map(|frame| RendererTerminalModeSnapshot {
            alt_screen: frame.alt_screen,
            cursor_keys_app: frame.cursor_keys_app,
            bracketed_paste: frame.bracketed_paste,
        }),
        reconciled_mode: state.reconciled_mode,
        canonical_cursor: canonical
            .map(|frame| (frame.cursor_visible, frame.cursor_row, frame.cursor_col)),
        painted_cursor_visible: state.painted_cursor_visible,
        painted_cursor_row: (state.painted_cursor_visible == Some(true))
            .then_some(state.painted_cursor_row)
            .filter(|row| *row >= 0),
        painted_cursor_col: (state.painted_cursor_visible == Some(true))
            .then_some(state.painted_cursor_col)
            .filter(|col| *col >= 0),
        cursor_connected: state.cursor_connected,
        canonical_cols: canonical.map(|frame| frame.cols),
        painted_cols: state.painted_cols,
        at_bottom: state.at_bottom,
        follows_bottom: state.follows_bottom,
    }
}

/// Two painted history rows are the same row when they paint the same cells.
///
/// Authoritative history that disagrees with a painted row forces a repair
/// rather than a merge: the worker is the only authority on immutable history,
/// so a disagreement means the DOM holds a row no frame ever described.
pub fn same_scrollback_row(left: &CellRow, right: &CellRow) -> bool {
    left.index == right.index && left.mark == right.mark && left.spans == right.spans
}

/// When a renderer boundary was crossed, for the incident recorder that reads
/// the DOM before a repair replaces the nodes carrying the evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererIncidentPhase {
    /// Before an incoming frame is applied.
    PreApply,
    /// Before a repair that replaces painted nodes.
    PreDestructive,
    /// Before a history page is spliced in.
    PreHistoryInsert,
    /// After a reconcile watermark advanced.
    PostReconcile,
}

/// How a frame reached the renderer at the moment the phase was crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererFrameMode {
    /// An authoritative full.
    Full,
    /// One or more sparse deltas.
    Delta,
}

/// Installed only by an armed terminal incident recorder. `armed` is read at
/// every renderer call site BEFORE an argument is constructed, so an unarmed
/// terminal reads no DOM and allocates nothing.
pub trait RendererIncidentObserver {
    /// Whether this observer wants the observation.
    fn armed(&self) -> bool;

    /// Called at a crossed boundary, before or after the repair it describes.
    fn observe(&mut self, phase: RendererIncidentPhase, mode: Option<RendererFrameMode>);
}
