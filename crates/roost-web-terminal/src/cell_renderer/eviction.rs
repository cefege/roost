//! Keeping the painted DOM bounded and the scroll space truthful: leading-block
//! eviction, head-gap collapse, and the head spacer that reserves the whole
//! session's history, so a dropped row keeps its exact height and native
//! `scrollTop` holds the reader's row with no application scroll write. Ports
//! `_evictScrollback`, `_collapseLeadingGaps`, `_syncSpacer` and `setHistoryFloor`
//! of `apps/web/src/renderer/cellRenderer.ts`.

use crate::block_placeholder::fixed_px;
use crate::cell_renderer::CellGridRenderer;
use crate::cell_renderer::scrollback::{BLOCK_CLASS, GAP_CLASS};
use crate::cell_renderer_dom::{effective_row_height, size_scrollback_block};
use crate::painted_history::{MAX_HELD_SCROLLBACK_ROWS, plan_eviction};
use crate::render_element::RenderElement;

impl<E: RenderElement> CellGridRenderer<E> {
    /// Drop whole leading blocks until the painted store is back inside its cap.
    ///
    /// Only a tail-following reader evicts: a reader parked in history is
    /// looking at rows this would remove from under them, and the store is
    /// bounded precisely so that case stays rare rather than impossible.
    pub(crate) fn evict_scrollback(&mut self, follow_tail: bool) {
        if !follow_tail {
            return;
        }
        while self.painted.len() > MAX_HELD_SCROLLBACK_ROWS {
            self.collapse_leading_gaps();
            let Some(lead) = self.scrollback.first_child() else {
                break;
            };
            if lead.class_name() != BLOCK_CLASS {
                break;
            }
            let leading_rows = lead.child_count();
            let Some(step) = plan_eviction(&self.painted, MAX_HELD_SCROLLBACK_ROWS, leading_rows)
            else {
                break;
            };
            if step.removes_block {
                lead.remove();
            } else {
                for _ in 0..step.rows {
                    if let Some(child) = lead.first_child() {
                        child.remove();
                    }
                }
                let row_height = self.row_height();
                size_scrollback_block(&lead, lead.child_count(), row_height);
            }
            if self.painted.evict_leading(step.rows as usize).is_none() {
                break;
            }
            if let Some(next_base) = step.next_base {
                self.painted_sb_base = next_base;
            }
        }
        self.collapse_leading_gaps();
        let Some(frame) = self.frame.as_mut().map(std::rc::Rc::make_mut) else {
            return;
        };
        if frame.scrollback_rows.len() > MAX_HELD_SCROLLBACK_ROWS {
            let excess = frame.scrollback_rows.len() - MAX_HELD_SCROLLBACK_ROWS;
            frame.scrollback_rows.drain(..excess);
            frame.sb_base = frame
                .scrollback_rows
                .first()
                .map_or(frame.scrollback_total, |row| u64::from(row.index));
        }
    }

    /// Remove head gaps the painted store has caught up with, so a reader who
    /// reaches the top of the painted history finds rows rather than reserved
    /// space for rows that are already there.
    fn collapse_leading_gaps(&mut self) {
        loop {
            let Some(lead) = self.scrollback.first_child() else {
                return;
            };
            if lead.class_name() != GAP_CLASS {
                return;
            }
            let Some(range) = self.gap_range(&lead) else {
                return;
            };
            if range.start != self.painted_sb_base {
                return;
            }
            self.gap_rows -= u64::from(range.end - range.start);
            if self.tail_gap.as_ref() == Some(&lead) {
                self.tail_gap = None;
            }
            self.painted_sb_base = range.end;
            lead.remove();
        }
    }

    /// Re-stamp the head spacer, which reserves the unpainted history ABOVE the
    /// painted base.
    ///
    /// The spacer is a SIBLING of the history sheet, so a prepend shrinks it by
    /// exactly what it painted, an eviction grows it by exactly what it dropped,
    /// and a reframe repaints the same rows at the same offsets. The scrollbar
    /// thumb therefore reflects the real session total, and a reader who drags
    /// into reserved-but-unpainted space keeps the backfill drain pulling toward
    /// them.
    pub(crate) fn sync_spacer(&mut self) {
        // The head reservation is the one pixel value every absolute row offset
        // is measured against, so it is stamped from a MEASURED height, falling
        // back to the default pitch — never from a stale or zeroed cache.
        let row_height = self.row_height();
        if row_height > 0.0 && row_height != self.painted_gap_row_height {
            self.resize_history_placeholders(row_height);
        }
        // Rows below a proven retention floor never arrive, so the spacer's
        // pending texture must stop claiming them — but only where the spacer
        // lies ENTIRELY below it: an interior floor leaves `[floor, sb_base)`
        // pageable, and splices move that base, so this is derived, never
        // latched.
        if self.history_floor_row > 0 && self.history_floor_row >= self.painted_sb_base {
            self.spacer.set_attribute("data-history-floor", "1");
        } else {
            self.spacer.remove_attribute("data-history-floor");
        }
        let height = fixed_px(f64::from(self.painted_sb_base) * effective_row_height(row_height));
        if height == self.painted_spacer_height {
            return;
        }
        self.spacer.set_style("height", &height);
        self.painted_spacer_height = height;
    }

    /// The pager owns the floor VALUE; the spacer's marker derives from it.
    pub fn set_history_floor(&mut self, row: u32) {
        if row == self.history_floor_row {
            return;
        }
        tracing::debug!(target: "scrollback", row, sb_base = self.painted_sb_base, "scrollback.history_floor_set");
        self.history_floor_row = row;
        self.sync_spacer();
    }
}
