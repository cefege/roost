//! What the pane registry may read off one mounted pane's renderer: painted
//! text, watermarks, and the painted history ranges the smoke probes check.
//! `CellGridRenderer` behind its `RefCell` is the production surface; a borrow
//! that is busy (a paint in progress) answers `None` rather than panicking.
//! Ports the reads `apps/web/src/renderer/terminalPreview.ts`'s registry entry
//! served to `apps/web/src/smoke/smokeTerminalRenderProbes.ts`.

use std::cell::RefCell;

use roost_protocol::cell::{CellGridFrame, CellRow, spans_text};
use roost_web_terminal::render_element::RenderElement;
use roost_web_terminal::{
    BackfillAnchor, CellGridRenderer, PaintedRowText, ReaderIntent, ReconcileBlockReason,
    RendererEpochSeq, RendererPaintPresentation, RendererPresentationSnapshot,
};

/// One painted row as text, addressed by its absolute history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedLine {
    /// Absolute row: history rows count from zero, viewport rows follow the
    /// frame's `scrollback_total`.
    pub row: u32,
    /// The row's painted text.
    pub text: String,
    /// Whether the row is a live viewport row rather than history.
    pub in_viewport: bool,
}

/// Where a marker was found in painted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaintedMarkerHit {
    /// The absolute row.
    pub row: u32,
    /// The marker's first column, counted in characters.
    pub column: u32,
    /// Whether the row is a live viewport row.
    pub in_viewport: bool,
}

/// The renderer watermarks and reader state one probe reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceProbe {
    /// How far canonical has advanced.
    pub canonical: RendererEpochSeq,
    /// How far the painted DOM has reconciled.
    pub reconciled: RendererEpochSeq,
    /// Live tail or parked reader.
    pub reader_intent: ReaderIntent,
    /// Whether the scroll box sits at its exact bottom.
    pub at_bottom: bool,
    /// Immutable history rows painted.
    pub painted_scrollback_rows: usize,
    /// Viewport row elements in the DOM.
    pub dom_rows: usize,
    /// Why the DOM is behind canonical, or `None` when it is not.
    pub reconcile_block_reason: ReconcileBlockReason,
    /// The painted history range and the epoch a history page must match.
    pub backfill_anchor: Option<BackfillAnchor>,
}

/// The read side of one mounted pane.
pub trait PaneSurface {
    /// The canonical viewport as text, one line per row.
    fn viewport_text(&self) -> Option<String>;
    /// The newest `max_rows` history rows of the applied frame as text.
    fn scrollback_text(&self, max_rows: usize) -> Option<String>;
    /// Every painted row, oldest history first, viewport rows last.
    fn painted_lines(&self) -> Option<Vec<PaintedLine>>;
    /// Watermarks and reader state.
    fn probe(&self) -> Option<SurfaceProbe>;
    /// The painted window around the reader, capped at `row_limit` rows.
    fn paint_presentation(&self, row_limit: Option<usize>) -> Option<RendererPaintPresentation>;
    /// Whether every row of `[start, end)` is painted.
    fn has_painted_scrollback_range(&self, start: u32, end: u32) -> bool;
    /// The painted text of `[start, end)`, or `None` when any row is unpainted.
    fn painted_scrollback_range(&self, start: u32, end: u32) -> Option<Vec<PaintedRowText>>;
    /// The presentation snapshot diagnostics diff.
    fn presentation_snapshot(&self) -> Option<RendererPresentationSnapshot>;
    /// The newest non-blank rows of the current frame, oldest first.
    fn preview_rows(&self) -> Option<Vec<CellRow>>;
}

/// Rows a tab-grid preview shows: at most this many non-blank rows.
pub const MAX_PREVIEW_ROWS: usize = 18;
/// History rows scanned beside the viewport for a preview.
pub const PREVIEW_CANDIDATE_SCAN: usize = 20;

/// The newest non-blank rows of a frame's history tail and viewport, oldest
/// first. An alt-screen frame has no history, so its viewport alone is the
/// content.
pub fn preview_rows_of(frame: &CellGridFrame) -> Vec<CellRow> {
    let tail = frame
        .scrollback_rows
        .len()
        .saturating_sub(PREVIEW_CANDIDATE_SCAN);
    let mut picked: Vec<CellRow> = frame.scrollback_rows[tail..]
        .iter()
        .chain(frame.viewport_rows.iter())
        .rev()
        .filter(|row| !spans_text(&row.spans).trim().is_empty())
        .take(MAX_PREVIEW_ROWS)
        .cloned()
        .collect();
    picked.reverse();
    picked
}

/// The first painted row, in order, whose text contains `marker`. An empty
/// marker matches nothing: every row would contain it.
pub fn find_marker(lines: &[PaintedLine], marker: &str) -> Option<PaintedMarkerHit> {
    if marker.is_empty() {
        return None;
    }
    lines.iter().find_map(|line| {
        let byte = line.text.find(marker)?;
        let column = line.text[..byte].chars().count();
        Some(PaintedMarkerHit {
            row: line.row,
            column: u32::try_from(column).unwrap_or(u32::MAX),
            in_viewport: line.in_viewport,
        })
    })
}

impl<E: RenderElement> PaneSurface for RefCell<CellGridRenderer<E>> {
    fn viewport_text(&self) -> Option<String> {
        Some(self.try_borrow().ok()?.grid_text())
    }

    fn scrollback_text(&self, max_rows: usize) -> Option<String> {
        Some(self.try_borrow().ok()?.scrollback_text(max_rows))
    }

    fn painted_lines(&self) -> Option<Vec<PaintedLine>> {
        let renderer = self.try_borrow().ok()?;
        let projection = renderer.renderer_projection();
        let mut lines: Vec<PaintedLine> = projection
            .painted_history
            .iter()
            .map(|row| PaintedLine {
                row: row.index,
                text: spans_text(&row.spans),
                in_viewport: false,
            })
            .collect();
        if let Some(frame) = renderer.current_frame() {
            let base = u32::try_from(frame.scrollback_total).unwrap_or(u32::MAX);
            lines.extend(frame.viewport_rows.iter().map(|row| PaintedLine {
                row: base.saturating_add(row.index),
                text: spans_text(&row.spans),
                in_viewport: true,
            }));
        }
        Some(lines)
    }

    fn probe(&self) -> Option<SurfaceProbe> {
        let renderer = self.try_borrow().ok()?;
        Some(SurfaceProbe {
            canonical: renderer.canonical_epoch_seq(),
            reconciled: renderer.reconciled_epoch_seq(),
            reader_intent: renderer.reader_intent(),
            at_bottom: renderer.at_bottom(),
            painted_scrollback_rows: renderer.painted_scrollback_row_count(),
            dom_rows: renderer.renderer_projection().dom_rows,
            reconcile_block_reason: renderer.reconcile_block_reason(),
            backfill_anchor: renderer.backfill_anchor(),
        })
    }

    fn paint_presentation(&self, row_limit: Option<usize>) -> Option<RendererPaintPresentation> {
        Some(self.try_borrow_mut().ok()?.paint_presentation(row_limit))
    }

    fn has_painted_scrollback_range(&self, start: u32, end: u32) -> bool {
        self.try_borrow()
            .is_ok_and(|renderer| renderer.has_painted_scrollback_range(start, end))
    }

    fn painted_scrollback_range(&self, start: u32, end: u32) -> Option<Vec<PaintedRowText>> {
        self.try_borrow().ok()?.painted_scrollback_range(start, end)
    }

    fn presentation_snapshot(&self) -> Option<RendererPresentationSnapshot> {
        Some(self.try_borrow().ok()?.presentation_snapshot())
    }

    fn preview_rows(&self) -> Option<Vec<CellRow>> {
        let renderer = self.try_borrow().ok()?;
        let frame = renderer.current_frame()?;
        (!frame.viewport_rows.is_empty()).then(|| preview_rows_of(frame))
    }
}
