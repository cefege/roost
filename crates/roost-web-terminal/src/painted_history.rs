//! The rows the renderer has actually painted, kept sorted by absolute index,
//! plus the eviction arithmetic that bounds them.
//!
//! The interval questions ("which rows are missing", "does this page land in a
//! gap") are NOT reimplemented here: `roost-client-core`'s `terminal::history`
//! owns that arithmetic and this store is the adapter that feeds it a sorted
//! index list. Two hand-maintained answers to "what is painted" is exactly the
//! defect that lets a second implementation of history arithmetic disagree with
//! the first.
//!
//! The store is deliberately NOT contiguous: a retained-but-unpainted gap
//! between two painted intervals is a first-class state, so a reserved gap
//! needs no inference to stand in for it.

use roost_client_core::terminal::history as intervals;
use roost_protocol::cell::CellRow;

/// Rows the renderer may hold. Past this the leading blocks are evicted and the
/// head spacer grows by exactly what they dropped, so the painted DOM cannot
/// grow without bound while the scroll space keeps describing the whole
/// session.
pub const MAX_HELD_SCROLLBACK_ROWS: usize = 2000;

/// The painted immutable history, sorted ascending by absolute row index.
#[derive(Debug, Clone, Default)]
pub struct PaintedHistory {
    rows: Vec<CellRow>,
    indices: Vec<u32>,
}

impl PaintedHistory {
    /// An empty store.
    pub const fn new() -> Self {
        Self {
            rows: Vec::new(),
            indices: Vec::new(),
        }
    }

    /// How many rows are painted.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether nothing is painted.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The painted rows, ascending.
    pub fn rows(&self) -> &[CellRow] {
        &self.rows
    }

    /// The painted absolute indices, ascending — the input every interval query
    /// takes.
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// The painted row at one absolute index, or `None` when it is unpainted.
    pub fn row_at(&self, index: u32) -> Option<&CellRow> {
        let position = intervals::insertion_index(&self.indices, index);
        self.indices
            .get(position)
            .filter(|painted| **painted == index)
            .map(|painted| &self.rows[position])
    }

    /// Splice one contiguous page in at its sorted position. The caller has
    /// already proved the page names exactly the interval it is filling; this
    /// only maintains the ordering.
    pub fn insert_page(&mut self, page: &[CellRow]) {
        let Some(first) = page.first() else {
            return;
        };
        let position = intervals::insertion_index(&self.indices, first.index);
        self.indices.splice(position..position, page.iter().map(|row| row.index));
        self.rows.splice(position..position, page.iter().cloned());
    }

    /// Drop `count` rows from the head, returning the new painted base.
    ///
    /// Returns `None` when the store holds fewer than `count` rows, which never
    /// happens for a caller that sized the drop from the store itself.
    pub fn evict_leading(&mut self, count: usize) -> Option<u32> {
        if count == 0 {
            return self.indices.first().copied();
        }
        if count > self.rows.len() {
            return None;
        }
        let next_base = self.indices.get(count - 1)?.checked_add(1);
        self.rows.drain(..count);
        self.indices.drain(..count);
        next_base
    }

    /// The missing interval containing one row, or `None` when it is painted.
    pub fn missing_range_at(
        &self,
        total: u32,
        row: u32,
    ) -> Option<intervals::HistoryRange> {
        intervals::missing_range_at(&self.indices, total, row)
    }

    /// The missing intervals inside one requested range, ascending.
    pub fn missing_ranges(
        &self,
        total: u32,
        start: u32,
        end: u32,
    ) -> Vec<intervals::HistoryRange> {
        intervals::missing_ranges(&self.indices, total, start, end)
    }

    /// Whether every row of a nonempty historical range is painted.
    pub fn has_range(&self, total: u32, start: u32, end: u32) -> bool {
        intervals::has_history_range(&self.indices, total, start, end)
    }

    /// The missing interval the reader's own scroll position exposes.
    pub fn missing_range_at_scroll(
        &self,
        total: u32,
        scroll_top: f64,
        spacer_top: f64,
        client_height: f64,
        row_height: f64,
        ahead_rows: u32,
    ) -> Option<intervals::HistoryScrollTarget> {
        intervals::missing_range_at_scroll(
            &self.indices,
            total,
            scroll_top,
            spacer_top,
            client_height,
            row_height,
            ahead_rows,
        )
    }
}

/// One step of the leading-block eviction, decided from the painted store
/// alone so the arithmetic is testable without a DOM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvictionStep {
    /// Rows to drop from the head of the store.
    pub rows: u32,
    /// The new painted base, or `None` when the store empties.
    pub next_base: Option<u32>,
    /// Whether the whole leading block goes with them, rather than being
    /// re-sized to the rows that stay.
    pub removes_block: bool,
}

/// Decide the next eviction step, or `None` when the store is within the cap or
/// the leading child is not a painted block.
///
/// A gap at the head is collapsed first by the caller, so the leading child's
/// row count is the ONLY DOM fact this needs — which is what makes the cap
/// arithmetic testable here rather than in a browser.
pub fn plan_eviction(
    painted: &PaintedHistory,
    cap: usize,
    leading_block_rows: u32,
) -> Option<EvictionStep> {
    if painted.len() <= cap || leading_block_rows == 0 {
        return None;
    }
    let excess = painted.len() - cap;
    let rows = u32::try_from(excess).unwrap_or(u32::MAX).min(leading_block_rows);
    if rows == 0 {
        return None;
    }
    let last_dropped = usize::try_from(rows).ok()?.checked_sub(1)?;
    let next_base = painted
        .indices()
        .get(last_dropped)
        .and_then(|last| last.checked_add(1));
    Some(EvictionStep {
        rows,
        next_base,
        removes_block: rows == leading_block_rows,
    })
}

/// Whether a page's rows name exactly one half-open interval, in order.
///
/// A page is safe to insert only when it IS the interval it claims: a short,
/// long, or out-of-order page would leave the painted set claiming coverage it
/// does not have, and every later interval query would answer from that lie.
pub fn page_is_contiguous(page: &[CellRow], start: u32, end: u32) -> bool {
    if start >= end || page.len() as u32 != end - start {
        return false;
    }
    page.iter()
        .enumerate()
        .all(|(offset, row)| row.index == start + offset as u32)
}
