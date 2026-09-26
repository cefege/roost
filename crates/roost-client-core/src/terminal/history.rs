//! Pure absolute scrollback interval arithmetic.
//!
//! A renderer owns the painted rows and the host owns the scroll box; neither
//! should have to infer what is covered from `sb_base`. These functions take the
//! painted row indices and the history total and answer which absolute rows are
//! missing, so a pager can ask for exactly those.
//!
//! Ported from `apps/web/src/client/terminal-stream/cellHistoryRanges.ts`, which
//! is pure and carries no renderer state. The rules are arithmetic, not policy:
//! absolute indices are compared, never recomputed, because history is trimmed at
//! a cap and a re-derived index re-aliases.

/// A half-open absolute interval `[start, end)` of history rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRange {
    /// The first row, inclusive.
    pub start: u32,
    /// The last row, exclusive.
    pub end: u32,
}

impl HistoryRange {
    /// How many rows the interval covers.
    pub const fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the interval covers nothing. A zero-length range is never a
    /// request: asking for zero rows is what a caller does when it has lost
    /// track of its own state, and answering it with an empty page hides that.
    pub const fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

/// The missing interval a reader's own scroll position exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryScrollTarget {
    /// The whole missing interval, which may extend past the visible window.
    pub missing: HistoryRange,
    /// The part inside the window the pager asked about, which is what it should
    /// prioritise — the reader cannot see rows outside its own box.
    pub in_window: HistoryRange,
    /// The row the reader is actually looking at, i.e. the start of the missing
    /// interval. Scrolling here is what the reader expects to happen.
    pub focus_row: u32,
}

/// True when a half-open absolute range belongs to one history total.
pub fn is_history_range(total: u32, start: u32, end: u32) -> bool {
    start < end && end <= total
}

/// True when the painted rows are sorted, unique, and inside the total.
///
/// Every interval query below assumes it. A caller that has appended a page out
/// of order would otherwise get an interval that says "nothing is missing" for a
/// region that is in fact unpainted.
pub fn has_sorted_history_rows(rows: &[u32], total: u32) -> bool {
    let mut previous: Option<u32> = None;
    for row in rows {
        if *row >= total {
            return false;
        }
        if previous.is_some_and(|value| *row <= value) {
            return false;
        }
        previous = Some(*row);
    }
    true
}

/// Lower-bound position for one absolute row in sorted painted history.
pub fn insertion_index(rows: &[u32], index: u32) -> usize {
    let mut low = 0usize;
    let mut high = rows.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if rows[middle] < index {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// The missing half-open interval containing `row`, or `None` when it is painted.
pub fn missing_range_at(rows: &[u32], total: u32, row: u32) -> Option<HistoryRange> {
    if row >= total {
        return None;
    }
    let insertion = insertion_index(rows, row);
    if rows.get(insertion) == Some(&row) {
        return None;
    }
    let start = rows
        .get(insertion.wrapping_sub(1))
        .map_or(0, |value| value + 1);
    let end = rows.get(insertion).copied().unwrap_or(total);
    (start < end).then_some(HistoryRange { start, end })
}

/// True only when every row in a nonempty historical range is painted.
pub fn has_history_range(rows: &[u32], total: u32, start: u32, end: u32) -> bool {
    if !is_history_range(total, start, end) {
        return false;
    }
    (insertion_index(rows, start)..)
        .zip(start..end)
        .all(|(position, row)| rows.get(position) == Some(&row))
}

/// The missing intervals inside one requested historical range, ascending.
///
/// Ordered because a pager issues them in order and a reader scrolling upward
/// wants the oldest gap first; an unordered set of gaps is a set of gaps whose
/// fetch order has to be re-decided per call.
pub fn missing_ranges(rows: &[u32], total: u32, start: u32, end: u32) -> Vec<HistoryRange> {
    if !is_history_range(total, start, end) {
        return Vec::new();
    }
    let mut gaps = Vec::new();
    let mut cursor = start;
    let mut position = insertion_index(rows, start);
    while cursor < end {
        match rows.get(position) {
            Some(painted) if *painted < end => {
                if *painted > cursor {
                    gaps.push(HistoryRange {
                        start: cursor,
                        end: *painted,
                    });
                }
                cursor = painted + 1;
                position += 1;
            }
            _ => {
                gaps.push(HistoryRange { start: cursor, end });
                break;
            }
        }
    }
    gaps
}

/// Whether a page's row shells name exactly this interval, in order.
///
/// A page is safe to insert only when its rows are the interval and nothing
/// else. A page that is short, long, or out of order would leave the painted
/// set claiming coverage it does not have, and every later interval query would
/// then answer from that lie.
pub fn is_contiguous_page(rows: &[u32], start: u32, end: u32) -> bool {
    if start >= end || rows.len() as u32 != end - start {
        return false;
    }
    rows.iter()
        .enumerate()
        .all(|(offset, row)| *row == start + offset as u32)
}

/// The absolute rows a scroll box shows, or `None` when it sits past the painted
/// history.
///
/// `scroll_top` and `spacer_top` are in the same pixel space, which is why the
/// subtraction happens before the division: the spacer is the unpainted history
/// above the painted rows, and forgetting it puts every row at the wrong index.
pub fn visible_row_range(
    scroll_top: f64,
    spacer_top: f64,
    client_height: f64,
    row_height: f64,
    total: u32,
) -> Option<HistoryRange> {
    if row_height <= 0.0 || client_height <= 0.0 {
        return None;
    }
    let first = ((scroll_top - spacer_top) / row_height).floor().max(0.0);
    let last = ((scroll_top + client_height - spacer_top) / row_height).ceil();
    let start = first.min(u32::MAX as f64) as u32;
    let end = last.max(0.0).min(total as f64) as u32;
    (start < end).then_some(HistoryRange { start, end })
}

/// The missing interval the reader's own scroll position exposes.
///
/// `ahead_rows` widens the window UPWARD, so a demand can precede the reader's
/// arrival: a reader who scrolls fast should find the rows above already there
/// rather than watching a gap open above them.
pub fn missing_range_at_scroll(
    rows: &[u32],
    total: u32,
    scroll_top: f64,
    spacer_top: f64,
    client_height: f64,
    row_height: f64,
    ahead_rows: u32,
) -> Option<HistoryScrollTarget> {
    let visible = visible_row_range(scroll_top, spacer_top, client_height, row_height, total)?;
    let window_start = visible.start.saturating_sub(ahead_rows);
    let in_window = missing_ranges(rows, total, window_start, visible.end).pop()?;
    let focus_row = in_window.start;
    let missing = missing_range_at(rows, total, focus_row)?;
    Some(HistoryScrollTarget {
        missing,
        in_window,
        focus_row,
    })
}
