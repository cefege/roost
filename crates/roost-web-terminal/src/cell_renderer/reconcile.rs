//! The canonical reconcile: making the painted DOM match the frame the
//! renderer has accepted, and advancing the watermark only when it provably
//! has.
//!
//! A reconcile is either a full repaint — the grid, the history sheet and the
//! reserved spacer are all replaced — or a repair of the parts a frame cannot
//! prove. Painted history content is only ever the worker's own rows: a
//! viewport-only checkpoint reserves the interval it pushed as an UNPAINTED gap
//! and lets the epoch-addressed backfill fill it on demand, because a
//! checkpoint cannot prove which rows left the grid, and guessing is how a
//! repainted TUI block freezes a stale generation into history.

use roost_protocol::cell::{CellGridFrame, CellRow};

use crate::cell_renderer::CellGridRenderer;
use crate::cell_renderer_dom::DomResult;
use crate::presentation::{RendererEpochSeq, RendererIncidentPhase, RendererTerminalModeSnapshot};
use crate::render_element::RenderElement;

impl<E: RenderElement> CellGridRenderer<E> {
    /// Bring the painted DOM in line with the accepted frame.
    ///
    /// `follow_tail` is the pre-mutation bottom capture: the follow band or an
    /// exact renderer-owned placement, never a widened clamp. Only
    /// `pin_to_bottom` may write `scrollTop`, and only when that capture was
    /// true — a non-bottom mutation never moves the reader.
    pub(crate) fn reconcile_canonical(
        &mut self,
        follow_tail: bool,
        force_pin: bool,
    ) -> DomResult<()> {
        let Some(frame) = self.frame.clone() else {
            return Ok(());
        };
        let same_grid = self.reconciled_grid_epoch.as_deref() == Some(frame.grid_epoch.as_str())
            && self.painted_cols == Some(frame.cols)
            && self.row_elements.len() == frame.rows as usize
            && self.painted_alt_screen == Some(frame.alt_screen);
        let should_pin = force_pin || (follow_tail && self.at_bottom_or_owned_placement());
        if !same_grid || frame.scrollback_total < self.scrollback_layout_end {
            return self.render_full(follow_tail, should_pin);
        }
        self.extend_scrollback_gap(frame.scrollback_total)?;
        // A full carries its history in `scrollback_rows`; a delta carries the
        // lines it pushed. Either way, only the frame's OWN rows are painted.
        let authoritative: &[CellRow] = if frame.full {
            &frame.scrollback_rows
        } else {
            &frame.scrollback_append
        };
        if !self.insert_authoritative_history(authoritative, follow_tail) {
            return self.render_full(follow_tail, should_pin);
        }
        self.sync_spacer();
        self.render_viewport_repair()?;
        self.set_grid_width();
        self.sync_alt_screen();
        self.pin_to_bottom(should_pin);
        self.mark_reconciled_if_current();
        Ok(())
    }

    /// Repaint everything: the grid, the history sheet and the reserved spacer.
    ///
    /// The spacer is synced BEFORE the history is wiped, so the scroll maximum
    /// never dips under a reader's `scrollTop` and dumps them into stale space
    /// on the way to the live bottom.
    pub(crate) fn render_full(&mut self, follow_tail: bool, should_pin: bool) -> DomResult<()> {
        let Some(frame) = self.frame.clone() else {
            return Ok(());
        };
        // Evidence of a painted-history defect must be read BEFORE this repair
        // replaces the nodes that carry it.
        self.observe(RendererIncidentPhase::PreDestructive, None);
        self.painted_sb_base =
            crate::cell_renderer::history_page::to_row_index(frame.scrollback_total);
        self.scrollback_layout_end = frame.scrollback_total;
        self.gap_rows = 0;
        self.tail_gap = None;
        self.sync_spacer();
        self.row_height.set(0.0);
        self.scrollback.clear_children();
        self.cur_block = None;
        self.cur_block_rows = 0;
        self.painted.clear();
        self.viewport.clear_children();
        self.row_elements.clear();
        self.row_hashes.clear();
        let history = frame.scrollback_rows.clone();
        self.insert_authoritative_history(&history, follow_tail);
        self.sync_spacer();
        self.render_viewport_repair()?;
        self.set_grid_width();
        self.sync_alt_screen();
        self.pin_to_bottom(should_pin);
        self.mark_reconciled_if_current();
        Ok(())
    }

    /// Advance the reconciled watermark, but only when the DOM provably carries
    /// this frame.
    ///
    /// Every refusal is a fact the caller already knows: a hold, a pending
    /// render, a row count or width the DOM does not have, a cursor the frame
    /// describes differently. Marking any of them reconciled would tell a stall
    /// watchdog there is nothing to repair while the grid is frozen.
    pub(crate) fn mark_reconciled_if_current(&mut self) {
        let Some(frame) = self.frame.as_ref() else {
            return;
        };
        let dom_is_current = self.reader_pending_frame.is_none()
            && !self.reader.holding()
            && !self.pending_render
            && self.row_elements.len() == frame.rows as usize
            && self.painted_cols == Some(frame.cols)
            && self.painted_alt_screen == Some(frame.alt_screen)
            && self.painted_cursor_visible == Some(frame.cursor_visible);
        if !dom_is_current {
            return;
        }
        if frame.cursor_visible {
            // Compare the column `update_cursor` INTENDED to paint: judging a
            // leading prediction against the frame's own column froze this
            // watermark for the whole length of a typing burst.
            let painted_column = self.predicted_col.unwrap_or(frame.cursor_col);
            if self.painted_cursor_row != i64::from(frame.cursor_row)
                || self.painted_cursor_col != i64::from(painted_column)
            {
                return;
            }
        }
        let first_reconcile = self.reconciled_grid_epoch.is_none() && self.reconciled_seq.is_none();
        self.reconciled_grid_epoch = Some(frame.grid_epoch.clone());
        self.reconciled_seq = Some(frame.seq);
        self.reconciled_mode = Some(RendererTerminalModeSnapshot {
            alt_screen: frame.alt_screen,
            cursor_keys_app: frame.cursor_keys_app,
            bracketed_paste: frame.bracketed_paste,
        });
        self.observe(RendererIncidentPhase::PostReconcile, None);
        if first_reconcile && let Some(callback) = self.on_first_reconcile.take() {
            callback();
        }
        if let Some(callback) = self.on_reconcile.as_ref() {
            callback();
        }
    }

    /// The newest frame, which is the reader-pending one while a park holds it.
    pub(crate) fn canonical_frame(&self) -> Option<&CellGridFrame> {
        self.reader_pending_frame.as_ref().or(self.frame.as_ref())
    }

    /// The canonical `(epoch, seq)` pair.
    pub(crate) fn canonical_watermark(&self) -> RendererEpochSeq {
        RendererEpochSeq {
            grid_epoch: self.canonical_frame().map(|frame| frame.grid_epoch.clone()),
            seq: self.canonical_frame().map(|frame| frame.seq),
        }
    }

    /// The reconciled `(epoch, seq)` pair, which stays behind the canonical one
    /// for as long as a hold or a park keeps the DOM immutable.
    pub(crate) fn reconciled_watermark(&self) -> RendererEpochSeq {
        RendererEpochSeq {
            grid_epoch: self.reconciled_grid_epoch.clone(),
            seq: self.reconciled_seq,
        }
    }

    /// Report the pre-history-insert boundary to an armed incident recorder.
    pub(crate) fn observe_history_insert(&mut self) {
        self.observe(RendererIncidentPhase::PreHistoryInsert, None);
    }

    /// The applied frame's retained scrollback total.
    pub(crate) fn frame_total(&self) -> u64 {
        self.frame
            .as_ref()
            .map_or(0, |frame| frame.scrollback_total)
    }
}
