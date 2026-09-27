//! The immutable history sheet: sealed 250-row blocks and the exact-height gaps
//! that stand in for rows nothing has painted yet.
//!
//! The DOM may retain DISJOINT immutable rows separated by those gaps. That is
//! what makes a demand backfill possible: an unpainted interval keeps its
//! reserved pixels and its scroll position, and the epoch-addressed page that
//! fills it lands without moving the reader. Reserving the space is also the
//! only honest answer to a checkpoint: a viewport-only full cannot prove which
//! rows left the grid, so the renderer never infers them.

use web_sys::Element;
use roost_protocol::cell::CellRow;

use crate::cell_renderer_dom::{DomResult, as_node, create_div, detach, size_scrollback_block};
use crate::block_placeholder::{SCROLLBACK_BLOCK_ROWS, block_placeholder};
use roost_client_core::terminal::history::HistoryRange;

/// The class on a block of painted rows. Its child count is the eviction unit.
pub(crate) const BLOCK_CLASS: &str = "cell-block";
/// The class on an exact-height gap standing in for unpainted rows.
pub(crate) const GAP_CLASS: &str = "cell-sb-gap";

impl CellGridRenderer {
    /// How many history lines the worker retains, as the interval arithmetic's
    /// total. Saturating, because the arithmetic compares against a `u32` and a
    /// total past that cannot name a row any browser painted.
    pub(crate) fn history_total(&self) -> u32 {
        self.frame
            .as_ref()
            .map_or(0, |frame| {
                u32::try_from(frame.scrollback_total).unwrap_or(u32::MAX)
            })
    }

    /// The current number of children in the history sheet.
    pub(crate) fn child_count(&self) -> u32 {
        self.scrollback.children().length()
    }

    /// One child of the history sheet, by position.
    pub(crate) fn child_at(&self, index: u32) -> Option<Element> {
        self.scrollback.children().get(index)
    }

    /// Close the OPEN tail block: stamp its exact height and let the browser
    /// skip it again.
    ///
    /// `retain_tail` keeps the element as the current block after sealing, which
    /// a prepend needs so the tail is restored before a backfill page lands
    /// above it and native anchoring preserves the reader's row.
    pub(crate) fn seal_current_block(&mut self, retain_tail: bool) {
        let Some(block) = self.cur_block.clone() else {
            return;
        };
        let rows = self.cur_block_rows;
        let row_height = self.row_height();
        size_scrollback_block(&block, rows, row_height);
        let style = block.style();
        style.remove_property("overflow-anchor");
        style.remove_property("content-visibility");
        if retain_tail {
            return;
        }
        self.cur_block = None;
        self.cur_block_rows = 0;
    }

    /// Re-stamp every block and gap placeholder for a new measured row height.
    ///
    /// A font swap changes the height of a row, and every scroll position in the
    /// pane is derived from these reserved pixels. A gap is sized from its own
    /// declared range; a block from its own child count.
    pub(crate) fn resize_history_placeholders(&mut self, row_height: f64) -> bool {
        for index in 0..self.child_count() {
            let Some(child) = self.child_at(index) else {
                continue;
            };
            if child.class_name() == GAP_CLASS {
                let rows = attribute_u32(&child, "data-end-row")
                    .saturating_sub(attribute_u32(&child, "data-start-row"));
                child
                    .style()
                    .set_property("height", &block_placeholder(rows, row_height));
            } else {
                size_scrollback_block(&child, child.children().length(), row_height);
            }
        }
        self.painted_gap_row_height = row_height;
        true
    }

    /// The interval a gap element reserves, or `None` when it names no valid
    /// range. A malformed gap is left alone rather than resized from a guess.
    pub(crate) fn gap_range(&self, gap: &Element) -> Option<HistoryRange> {
        let start = attribute_u32(gap, "data-start-row");
        let end = attribute_u32(gap, "data-end-row");
        (start < end).then_some(HistoryRange { start, end })
    }

    /// Name and size one gap's interval. The height is recomputed from the live
    /// row pitch, so a font swap re-reserves the same rows at their new height.
    pub(crate) fn set_gap_range(&mut self, gap: &Element, start: u32, end: u32) {
        let _ = gap.set_attribute("data-start-row", &start.to_string());
        let _ = gap.set_attribute("data-end-row", &end.to_string());
        let row_height = self.row_height();
        gap.style()
            .set_property("height", &block_placeholder(end - start, row_height));
    }

    pub(crate) fn create_gap(&mut self, start: u32, end: u32) -> DomResult<Element> {
        let gap = create_div(&self.doc)?;
        gap.set_class_name(GAP_CLASS);
        gap.style().set_property("overflow-anchor", "none");
        self.set_gap_range(&gap, start, end);
        Ok(gap)
    }

    /// Append rows into blocks before `reference`, opening a new tail block when
    /// the rows run past the current one.
    fn insert_page_blocks(
        &mut self,
        rows: &[CellRow],
        reference: Option<&Element>,
        reuse_tail: bool,
        opens_tail: bool,
    ) -> DomResult<()> {
        let mut offset = 0usize;
        if reuse_tail
            && let Some(block) = self.cur_block.clone()
        {
            block.style().set_property("overflow-anchor", "none");
            block.style().set_property("content-visibility", "visible");
            while offset < rows.len() && self.cur_block_rows < SCROLLBACK_BLOCK_ROWS {
                let row = self.render_scrollback_row(&rows[offset])?;
                block.append_child(&row).ok();
                self.cur_block_rows += 1;
                offset += 1;
            }
            let block_rows = self.cur_block_rows;
            let row_height = self.row_height();
            size_scrollback_block(&block, block_rows, row_height);
            if block_rows == SCROLLBACK_BLOCK_ROWS {
                self.seal_current_block(false);
            }
        }
        let mut last_block: Option<Element> = None;
        let mut last_rows = 0u32;
        while offset < rows.len() {
            let block = create_div(&self.doc)?;
            block.set_class_name(BLOCK_CLASS);
            self.scrollback
                .insert_before(&block, reference.map(as_node))
                .ok();
            let mut block_rows = 0u32;
            while offset < rows.len() && block_rows < SCROLLBACK_BLOCK_ROWS {
                let row = self.render_scrollback_row(&rows[offset])?;
                block.append_child(&row).ok();
                block_rows += 1;
                offset += 1;
            }
            let row_height = self.row_height();
            size_scrollback_block(&block, block_rows, row_height);
            last_block = Some(block);
            last_rows = block_rows;
        }
        if opens_tail
            && let Some(block) = last_block
            && last_rows < SCROLLBACK_BLOCK_ROWS
        {
            // The OPEN tail stays out of content-visibility until it seals: a
            // skipped subtree's intrinsic size is re-evaluated at
            // rendering-lifecycle time, not on append, so appending into a
            // locked tail leaves the scroll height stale and every bottom check
            // reads a bottom that no longer exists.
            block.style().set_property("overflow-anchor", "none");
            block.style().set_property("content-visibility", "visible");
            self.cur_block = Some(block);
            self.cur_block_rows = last_rows;
        }
        Ok(())
    }

    /// Fill one reserved interval with the rows that belong in it.
    ///
    /// A page that spans the painted head base and the gap above it is refused:
    /// it has no single placeholder to land in, and a reader dragged to the top
    /// of history is exactly where that shape arises.
    pub(crate) fn insert_page_into_placeholder(
        &mut self,
        rows: &[CellRow],
        start: u32,
        end: u32,
    ) -> DomResult<bool> {
        let head_end = self.painted_sb_base;
        if start < head_end {
            if end > head_end {
                return Ok(false);
            }
            let first = self.scrollback.first_element_child();
            let tail_target = u64::from(head_end) == self.scrollback_layout_end;
            if self.cur_block.is_some() && !tail_target {
                self.seal_current_block(true);
            }
            self.insert_page_blocks(rows, first.as_ref(), false, tail_target)?;
            self.painted_sb_base = start;
            if end < head_end {
                let right = self.create_gap(end, head_end)?;
                self.scrollback
                    .insert_before(&right, first.as_ref().map(as_node))
                    .ok();
                self.gap_rows += u64::from(head_end - end);
                if tail_target {
                    self.tail_gap = Some(right);
                }
            } else if tail_target {
                self.tail_gap = None;
            }
            return Ok(true);
        }
        for index in 0..self.child_count() {
            let Some(gap) = self.child_at(index) else {
                continue;
            };
            if gap.class_name() != GAP_CLASS {
                continue;
            }
            let Some(range) = self.gap_range(&gap) else {
                continue;
            };
            if start < range.start || end > range.end {
                continue;
            }
            let next = self
                .child_at(index + 1)
                .or_else(|| self.scrollback.first_element_child());
            let tail_target = u64::from(range.end) == self.scrollback_layout_end;
            let reuse_tail = tail_target && start == range.start && self.cur_block.is_some();
            if !tail_target && self.cur_block.is_some() {
                self.seal_current_block(true);
            }
            self.gap_rows -= u64::from(end - start);
            if start == range.start {
                if end < range.end {
                    self.set_gap_range(&gap, end, range.end);
                    self.insert_page_blocks(rows, Some(&gap), reuse_tail, tail_target)?;
                    if tail_target {
                        self.tail_gap = Some(gap);
                    }
                } else {
                    detach(&gap);
                    self.insert_page_blocks(rows, next.as_ref(), reuse_tail, tail_target)?;
                    if tail_target {
                        self.tail_gap = None;
                    }
                }
            } else {
                if tail_target && self.cur_block.is_some() {
                    self.seal_current_block(false);
                }
                self.set_gap_range(&gap, range.start, start);
                self.insert_page_blocks(rows, next.as_ref(), false, tail_target)?;
                if end < range.end {
                    let right = self.create_gap(end, range.end)?;
                    self.scrollback
                        .insert_before(&right, next.as_ref().map(as_node))
                        .ok();
                    if tail_target {
                        self.tail_gap = Some(right);
                    }
                } else if tail_target {
                    self.tail_gap = None;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }
}

/// Read a `data-*` row bound off a gap element, defaulting to zero for a
/// missing or malformed attribute: a gap with no range is one this renderer
/// never sized, and guessing a range for it would claim pixels nobody reserved.
pub(crate) fn attribute_u32(element: &Element, name: &str) -> u32 {
    element
        .get_attribute(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}
