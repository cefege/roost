//! Admitting one page of history rows into the painted set: a backfilled page
//! the reader asked for, and a frame's own authoritative rows.
//!
//! Two admission rules, and they are the same rule. A page is painted only when
//! it names exactly the interval it is filling, its rows are contiguous and in
//! order, and it lies inside the layout the current frame describes. A page
//! that fails is REFUSED, never approximated — the painted set would otherwise
//! claim coverage it does not have, and every later interval query would answer
//! from that lie.

use web_sys::Element;

use crate::cell_renderer::CellGridRenderer;
use crate::cell_renderer_dom::DomResult;
use crate::painted_history::page_is_contiguous;
use crate::presentation::same_scrollback_row;
use roost_protocol::cell::CellRow;

impl CellGridRenderer {
    /// Splice one backfilled page into the painted history.
    ///
    /// The page is admitted only when it lands in a gap the current layout
    /// actually reserves; a page arriving for an interval that is already
    /// painted, or for rows past the frame's own total, is refused so the next
    /// demand can be re-derived from live state rather than from a stale one.
    pub(crate) fn insert_history_page(&mut self, rows: &[CellRow], follow_tail: bool) -> bool {
        self.observe_history_insert();
        self.measure_row_height();
        let Some(frame_total) = self.frame.as_ref().map(|frame| frame.scrollback_total) else {
            return false;
        };
        let Some(&start) = rows.first() else {
            return false;
        };
        let Some(end) = start.checked_add(u32::try_from(rows.len()).unwrap_or(u32::MAX)) else {
            return false;
        };
        if self.scrollback_layout_end != frame_total || !page_is_contiguous(rows, start, end) {
            return false;
        }
        let Some(missing) = self.painted.missing_range_at(self.history_total(), start) else {
            return false;
        };
        if end > missing.end {
            return false;
        }
        if !matches!(self.insert_page_into_placeholder(rows, start, end), Ok(true)) {
            return false;
        }
        self.painted.insert_page(rows);
        if follow_tail {
            self.evict_scrollback(true);
        }
        self.sync_spacer();
        true
    }

    /// Fill every missing interval inside an authoritative frame's own history.
    ///
    /// A full carries its history in `scrollback_rows`; a delta carries the
    /// lines it pushed in `scrollback_append`. Either way the rows are the
    /// worker's, and a painted row that DISAGREES with them forces a repair
    /// rather than a merge: the worker is the only authority on immutable
    /// history, and two answers means the DOM holds a row no frame described.
    pub(crate) fn insert_authoritative_history(
        &mut self,
        rows: &[CellRow],
        follow_tail: bool,
    ) -> bool {
        self.observe_history_insert();
        if rows.is_empty() {
            return true;
        }
        let Some(frame_total) = self.frame.as_ref().map(|frame| frame.scrollback_total) else {
            return false;
        };
        let Some(&start) = rows.first() else {
            return false;
        };
        let Some(end) = start.checked_add(u32::try_from(rows.len()).unwrap_or(u32::MAX)) else {
            return false;
        };
        if self.scrollback_layout_end != frame_total
            || !page_is_contiguous(rows, start, end)
            || u64::from(end) > frame_total
        {
            return false;
        }
        for row in rows {
            if let Some(painted) = self.painted.row_at(row.index)
                && !same_scrollback_row(painted, row)
            {
                return false;
            }
        }
        let missing = self
            .painted
            .missing_ranges(self.history_total(), start, end);
        for range in missing {
            let from = usize::try_from(range.start - start).unwrap_or(0);
            let through = usize::try_from(range.end - start).unwrap_or(0);
            let page = rows.get(from..through).unwrap_or_default();
            if !self.insert_history_page(page, follow_tail && range.end == end) {
                return false;
            }
        }
        true
    }

    /// Reserve the interval a checkpoint pushed but did not author.
    ///
    /// This is the rule that keeps a repainted TUI block out of history. The
    /// renderer never infers which rows left the grid — equal epoch, width and
    /// alt-screen are facts about the GRID and prove nothing about row CONTENT
    /// — so the interval waits, with its exact pixel height reserved, for the
    /// epoch-addressed page that says what is in it.
    pub(crate) fn extend_scrollback_gap(&mut self, end: u64) -> DomResult<()> {
        if end <= self.scrollback_layout_end {
            return Ok(());
        }
        let start = self.scrollback_layout_end;
        let mut reused = false;
        if let Some(gap) = self.tail_gap.clone()
            && let Some(range) = self.gap_range(&gap)
            && u64::from(range.end) == start
        {
            self.set_gap_range(&gap, range.start, to_row_index(end));
            reused = true;
        }
        if !reused {
            let tail = self.create_gap(to_row_index(start), to_row_index(end))?;
            self.scrollback.append_child(&tail).ok();
            self.tail_gap = Some(tail);
        }
        self.gap_rows += end - start;
        self.scrollback_layout_end = end;
        Ok(())
    }
}

/// An absolute row index, saturating: a history total past `u32::MAX` cannot
/// name a row any browser painted, and a saturating index keeps every derived
/// pixel offset in range instead of wrapping.
pub(crate) fn to_row_index(absolute: u64) -> u32 {
    u32::try_from(absolute).unwrap_or(u32::MAX)
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
