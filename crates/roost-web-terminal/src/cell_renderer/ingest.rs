//! Applying frames: one authoritative full, or one batch of sparse deltas.
//! A parked reader's frame is retained as the CANONICAL one and deltas fold
//! into it while the DOM stays what the reader is looking at — why canonical
//! and painted are two watermarks. Ports `apply`, `applyFullFrame`,
//! `applyDeltaFrames` and `_canRetainPaintedHistory` of `apps/web/src/renderer/cellRenderer.ts`.

use std::rc::Rc;

use roost_protocol::cell::{
    CellDeltaBatch, CellGridFrame, CellRow, clone_cell_grid_frame, fold_cell_delta_batch,
};

use crate::cell_renderer::CellGridRenderer;
use crate::presentation::{RendererFrameMode, RendererIncidentPhase};
use crate::reader_intent::{ReaderIntent, ReaderIntentReason};
use crate::render_element::RenderElement;

impl<E: RenderElement> CellGridRenderer<E> {
    /// Apply either an authoritative full or one sparse delta.
    pub fn apply(&mut self, incoming: &CellGridFrame) -> bool {
        if incoming.full {
            self.apply_full_frame(incoming)
        } else {
            self.apply_delta_frames(std::slice::from_ref(incoming))
        }
    }

    /// Apply an authoritative full while preserving a compatible painted history.
    ///
    /// A full is only admissible when it is COMPLETE: every viewport row
    /// present, numbered 0..rows-1. A sparse frame claiming `full` would let a
    /// partial checkpoint overwrite a whole grid, and nothing afterwards could
    /// prove the rows it dropped.
    pub fn apply_full_frame(&mut self, incoming: &CellGridFrame) -> bool {
        self.observe(
            RendererIncidentPhase::PreApply,
            Some(RendererFrameMode::Full),
        );
        if !incoming.full || incoming.viewport_rows.len() != incoming.rows as usize {
            return false;
        }
        if incoming
            .viewport_rows
            .iter()
            .enumerate()
            .any(|(index, row)| row.index != index as u32)
        {
            return false;
        }
        let owned = clone_cell_grid_frame(incoming);
        let retains_history = self.can_retain_painted_history(&owned);
        if self.reader.intent() == ReaderIntent::Reading || self.reader_pending_frame.is_some() {
            self.reader_pending_frame = Some(Rc::new(owned));
            self.reader_pending_frame_retains_history = retains_history;
            if self.reader.intent() == ReaderIntent::Live {
                self.pending_render = true;
            }
            if self
                .reader
                .reason()
                .is_some_and(ReaderIntentReason::is_position_only)
            {
                self.settle_bottom_park();
            }
            return true;
        }
        self.frame = Some(Rc::new(owned));
        if self.reader.holding() {
            self.pending_render = true;
            return true;
        }
        self.reconcile_canonical(true, false).is_ok()
    }

    /// Apply one batch of sparse deltas, folding them onto whichever frame is
    /// canonical right now.
    ///
    /// The base is the reader-pending frame when there is one, so a delta that
    /// arrives during a park continues the canonical chain instead of
    /// resurrecting a superseded one. An invalid chain is refused whole: a
    /// batch that half-applied would paint a grid no frame ever described.
    pub fn apply_delta_frames(&mut self, deltas: &[CellGridFrame]) -> bool {
        self.observe(
            RendererIncidentPhase::PreApply,
            Some(RendererFrameMode::Delta),
        );
        let Some(base) = self
            .reader_pending_frame
            .as_deref()
            .or(self.frame.as_deref())
        else {
            return false;
        };
        if base.viewport_rows.len() != base.rows as usize
            || (self.reader.intent() == ReaderIntent::Live
                && !self.reader.holding()
                && self.row_elements.len() != base.rows as usize)
        {
            return false;
        }
        let Some(batch) = fold_cell_delta_batch(base, deltas) else {
            return false;
        };
        let CellDeltaBatch {
            mut frame,
            dirty_rows,
            scrollback_append,
            viewport_shift,
        } = batch;
        let folded_from_pending = self.reader_pending_frame.is_some();
        if self.reader.intent() == ReaderIntent::Reading || folded_from_pending {
            let history = if folded_from_pending {
                take_history(&mut self.reader_pending_frame)
            } else {
                // Entering a reading park: the painted frame stays put, so its
                // history is copied once here rather than moved.
                self.frame
                    .as_ref()
                    .map(|painted| painted.scrollback_rows.clone())
                    .unwrap_or_default()
            };
            attach_history(history, &mut frame);
            self.reader_pending_frame = Some(Rc::new(frame));
            if self.reader.intent() == ReaderIntent::Live {
                self.pending_render = true;
            }
            if self
                .reader
                .reason()
                .is_some_and(ReaderIntentReason::is_position_only)
            {
                self.settle_bottom_park();
            }
            return true;
        }
        let was_at_bottom = self.at_bottom_or_owned_placement();
        attach_history(take_history(&mut self.frame), &mut frame);
        self.frame = Some(Rc::new(frame));
        if self.reader.holding() {
            self.pending_render = true;
            return true;
        }
        if self.extend_scrollback_gap(self.frame_total()).is_err() {
            return false;
        }
        if !scrollback_append.is_empty() && !self.insert_history_page(&scrollback_append, true) {
            return self.render_full(was_at_bottom, was_at_bottom).is_ok();
        }
        if self.render_delta(&dirty_rows, viewport_shift).is_err() {
            return false;
        }
        self.set_grid_width();
        self.sync_alt_screen();
        self.pin_to_bottom(was_at_bottom);
        self.mark_reconciled_if_current();
        self.paint_images();
        true
    }

    /// Whether a frame is compatible with the painted history it would meet.
    ///
    /// Every admission fact is about the GRID, never about row content: equal
    /// epoch, columns, row count and alt-screen prove the shapes agree, and the
    /// history only extends. They cannot prove which rows left the grid, which
    /// is exactly why a checkpoint reserves a gap instead of inferring rows.
    pub(crate) fn can_retain_painted_history(&self, frame: &CellGridFrame) -> bool {
        let Some(reconciled) = self.reconciled_grid_epoch.as_deref() else {
            return false;
        };
        if reconciled != frame.grid_epoch
            || self.painted_cols != Some(frame.cols)
            || self.row_elements.len() != frame.rows as usize
            || self.painted_alt_screen != Some(frame.alt_screen)
        {
            return false;
        }
        frame.scrollback_total >= self.scrollback_layout_end
    }

    /// Report a crossed boundary to an armed incident recorder, before the
    /// repair it describes replaces the nodes that carry the evidence.
    pub(crate) fn observe(
        &mut self,
        phase: RendererIncidentPhase,
        mode: Option<RendererFrameMode>,
    ) {
        if let Some(observer) = self.incident_observer.as_mut()
            && observer.armed()
        {
            observer.observe(phase, mode);
        }
    }
}

/// The base's history, moved out when this renderer held the only reference.
/// Every other holder of the frame (a reconcile or probe projection) lets go
/// before the next paint, so the copy is the exception.
fn take_history(slot: &mut Option<Rc<CellGridFrame>>) -> Vec<CellRow> {
    match slot.take().map(Rc::try_unwrap) {
        Some(Ok(frame)) => frame.scrollback_rows,
        Some(Err(shared)) => shared.scrollback_rows.clone(),
        None => Vec::new(),
    }
}

/// `history ++ the rows the batch appended`: the fold returns only the latter.
fn attach_history(mut history: Vec<CellRow>, frame: &mut CellGridFrame) {
    history.append(&mut frame.scrollback_rows);
    frame.scrollback_rows = history;
}
