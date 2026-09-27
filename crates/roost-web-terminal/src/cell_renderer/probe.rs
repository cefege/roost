//! The values a terminal pane publishes about itself: the epoch watermarks, the
//! backfill anchor, the painted-history range, and the two presentations the
//! smoke API and the diagnostic scanner read.
//!
//! None of these mutate. They exist so a consumer can answer "is this pane
//! behind, and by how much" from the renderer's own model rather than from a
//! screenshot, which is the question every diagnosis of a stalled pane actually
//! turns on.

use std::collections::BTreeMap;

use roost_protocol::cell::{CellRow, spans_text};

use crate::cell_renderer::CellGridRenderer;
use crate::cell_renderer_dom::{cell_grid_text, cell_scrollback_text, is_child_of};
use crate::presentation::{
    BackfillAnchor, PaintedRowText, RendererEpochSeq, RendererPaintPresentation,
    RendererPresentationSnapshot, RendererProjection, create_renderer_paint_presentation,
    create_renderer_presentation_snapshot,
};
use crate::reader_intent::ReconcileBlockReason;

impl CellGridRenderer {
    /// The sequence of the newest frame the renderer has accepted, including one
    /// held back for a parked reader. Zero before the first frame.
    pub fn canonical_frame_seq(&self) -> u64 {
        self.canonical_frame().map_or(0, |frame| frame.seq)
    }

    /// How far canonical has advanced.
    pub fn canonical_epoch_seq(&self) -> RendererEpochSeq {
        self.canonical_watermark()
    }

    /// How far the painted DOM has been reconciled. These stay separate across
    /// a hold or a park, and the gap between them IS the stall.
    pub fn reconciled_epoch_seq(&self) -> RendererEpochSeq {
        self.reconciled_watermark()
    }

    /// Why the DOM is behind the canonical frame, or `None` when it is not.
    pub fn reconcile_block_reason(&self) -> ReconcileBlockReason {
        let canonical = self.canonical_watermark();
        let reconciled = self.reconciled_watermark();
        self.reader.reconcile_block_reason(
            self.reader_pending_frame.is_some(),
            self.pending_render,
            (canonical.grid_epoch.as_deref(), canonical.seq),
            (reconciled.grid_epoch.as_deref(), reconciled.seq),
        )
    }

    /// The immutable grid identity and absolute range a history page must match,
    /// or `None` while no frame is applied or a pending frame has invalidated
    /// the painted base.
    pub fn backfill_anchor(&self) -> Option<BackfillAnchor> {
        let frame = self.frame.as_ref()?;
        if self.reader_pending_frame.is_some() && !self.reader_pending_frame_retains_history {
            return None;
        }
        Some(BackfillAnchor {
            sb_base: self.painted_sb_base,
            cols: frame.cols,
            total: frame.scrollback_total,
            grid_epoch: frame.grid_epoch.clone(),
        })
    }

    /// Whether every row of a half-open absolute range is painted.
    pub fn has_painted_scrollback_range(&self, start: u32, end: u32) -> bool {
        let Some(anchor) = self.backfill_anchor() else {
            return false;
        };
        self.scrollback_layout_end == anchor.total
            && self
                .painted
                .has_range(u32::try_from(anchor.total).unwrap_or(u32::MAX), start, end)
    }

    /// The painted text of a half-open absolute range, or `None` when any of it
    /// is unpainted. A partial answer would be a lie about coverage.
    pub fn painted_scrollback_range(&self, start: u32, end: u32) -> Option<Vec<PaintedRowText>> {
        if !self.has_painted_scrollback_range(start, end) {
            return None;
        }
        Some(
            self.painted
                .rows()
                .iter()
                .filter(|row| row.index >= start && row.index < end)
                .map(|row| PaintedRowText {
                    index: row.index,
                    text: spans_text(&row.spans),
                })
                .collect(),
        )
    }

    /// How many immutable history rows are painted.
    pub fn painted_scrollback_row_count(&self) -> usize {
        self.painted.len()
    }

    /// What a paint actually shows: a window of painted history centred on the
    /// reader, plus the two reserved pixel bands.
    pub fn paint_presentation(&mut self, row_limit: Option<usize>) -> RendererPaintPresentation {
        if self.reader.intent() == crate::reader_intent::ReaderIntent::Reading {
            self.capture_reader_anchor();
        }
        create_renderer_paint_presentation(
            self.painted.rows(),
            &self.painted_spacer_height,
            self.gap_rows,
            self.row_height(),
            self.reader_anchor,
            row_limit,
        )
    }

    /// Every renderer internal, as one read-only value. Callers never mutate
    /// what it returns; the painted history inside it is the live painted model.
    pub fn renderer_projection(&self) -> RendererProjection {
        let canonical = self.canonical_frame().cloned();
        let cursor_connected =
            is_child_of(&self.cursor, &self.viewport) && self.container.is_connected();
        RendererProjection {
            canonical,
            applied: self.frame.clone(),
            canonical_watermark: self.canonical_watermark(),
            reconciled_watermark: self.reconciled_watermark(),
            reader_intent: self.reader.intent(),
            reader_reason: self.reader.reason(),
            reader_anchor: self.reader_anchor,
            hold_mask: self.reader.hold_mask(),
            dom_rows: self.row_elements.len(),
            reconciled_mode: self.reconciled_mode,
            painted_cursor_visible: self.painted_cursor_visible,
            painted_cursor_row: self.painted_cursor_row,
            painted_cursor_col: self.painted_cursor_col,
            cursor_connected,
            painted_cols: self.painted_cols,
            at_bottom: self.at_bottom(),
            follows_bottom: self.follows_bottom(),
            painted_history: self.painted.rows().to_vec(),
            painted_sb_base: self.painted_sb_base,
            scrollback_layout_end: self.scrollback_layout_end,
            painted_spacer_height: self.painted_spacer_height.clone(),
            gap_rows: self.gap_rows,
            row_height: self.row_height(),
            default_row_height: crate::block_placeholder::DEFAULT_CELL_ROW_PX,
            scroll_top: self.scroll_top(),
            scroll_height: self.scroll_height(),
            client_height: self.client_height(),
        }
    }

    /// The presentation snapshot the smoke API publishes and diagnostics diff.
    pub fn presentation_snapshot(&self) -> RendererPresentationSnapshot {
        let projection = self.renderer_projection();
        create_renderer_presentation_snapshot(&projection, self.now_ms())
    }

    /// The canonical grid as text, one line per viewport row.
    pub fn grid_text(&self) -> String {
        cell_grid_text(self.canonical_frame())
    }

    /// The tail of the applied frame's history as text, newest `max_rows` rows.
    pub fn scrollback_text(&self, max_rows: usize) -> String {
        cell_scrollback_text(self.frame.as_ref(), max_rows)
    }

    /// The find highlights currently painted, keyed by absolute history row.
    pub fn find_highlights(&self) -> &BTreeMap<u32, Vec<crate::cell_row::FindHit>> {
        &self.find_hits
    }

    /// The active find match, as `(absolute row, column)`.
    pub fn active_find_hit(&self) -> Option<(u32, u32)> {
        self.active_hit
    }

    /// The canonical frame's history rows, for a caller comparing them against
    /// what is painted.
    pub fn canonical_history_rows(&self) -> &[CellRow] {
        self.canonical_frame()
            .map_or(&[], |frame| frame.scrollback_rows.as_slice())
    }

    /// The pane's clock, in milliseconds since the page's time origin. Zero
    /// before the window exists, so a snapshot taken then reports a real
    /// "no reading available" rather than a fabricated timestamp.
    fn now_ms(&self) -> f64 {
        web_sys::window()
            .and_then(|window| window.performance())
            .map_or(0.0, |performance| performance.now())
    }
}
