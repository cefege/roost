//! One scrollback history read, the page it may be answered with, and the
//! guards that decide whether that page may paint. `backfill::wave` owns what
//! the pager does with an admitted page. Ports the request/`validatePage`/
//! `noteFloor` half of `apps/web/src/renderer/scrollbackBackfill.ts`: a page is
//! admissible only as the interval asked for, under the grid the pane is
//! numbered as right now — otherwise a painted row disagrees with its frame.

use roost_client_core::terminal::history_backfill::DemandBounds;
use roost_protocol::cell::CellRow;
use roost_protocol::terminal_search::ScrollbackHistoryFloor;

/// One `SessionsGetScrollbackCells` query: rows ending at `end_row`, one epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackPageRequest {
    /// The session whose history is read.
    pub session_id: String,
    /// The newest row the page may contain, exclusive. A request is named by
    /// its NEWEST row because a page clamped at the retained floor comes back
    /// short at the OLD end.
    pub end_row: u32,
    /// The most rows the page may carry.
    pub max_rows: u32,
    /// The grid numbering the page must be addressed to.
    pub grid_epoch: String,
}

impl ScrollbackPageRequest {
    /// The query one demand becomes.
    pub(crate) fn for_demand(session_id: &str, demand: &Demand) -> Self {
        Self {
            session_id: session_id.to_string(),
            end_row: demand.bounds.end,
            max_rows: demand.bounds.end - demand.bounds.start,
            grid_epoch: demand.grid_epoch.clone(),
        }
    }
}

/// One history page as the carrier answered it, rows already decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackPage {
    /// The rows, which must name `[start_row, end_row)` exactly and in order.
    pub rows: Vec<CellRow>,
    /// The first row, inclusive, as the wire carries it.
    pub start_row: u64,
    /// The row after the last one, as the wire carries it.
    pub end_row: u64,
    /// The columns the rows were laid out in.
    pub cols: u32,
    /// The retained scrollback lines the worker holds now.
    pub scrollback_total: u64,
    /// The grid numbering the rows belong to.
    pub grid_epoch: String,
    /// Which retention floor a short page hit, and why.
    pub history_floor: ScrollbackHistoryFloor,
}

/// What raised a demand: a reader gesture, or a find match's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DemandKind {
    Scroll,
    Find,
}

impl DemandKind {
    /// The name the incident log reads.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Scroll => "scroll",
            Self::Find => "find",
        }
    }
}

/// One wave's rows plus the fence its answer must match: the right rows under
/// the wrong grid are still wrong, so a page is judged against both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Demand {
    pub(crate) bounds: DemandBounds,
    /// The pager generation that raised it, which also names the wave.
    pub(crate) generation: u64,
    pub(crate) kind: DemandKind,
    pub(crate) grid_epoch: String,
    pub(crate) cols: u32,
    /// The history total the anchor reported when the demand was raised.
    pub(crate) minimum_total: u64,
}

/// The guard that refused a page, in v2's check order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkGuard {
    Epoch,
    Cols,
    Total,
    StartRow,
    EndRow,
    RowCount,
    RowIndex,
}

impl ChunkGuard {
    /// The name the incident log reads.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Epoch => "epoch",
            Self::Cols => "cols",
            Self::Total => "total",
            Self::StartRow => "start_row",
            Self::EndRow => "end_row",
            Self::RowCount => "row_count",
            Self::RowIndex => "row_index",
        }
    }
}

/// A page that passed every guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedPage {
    pub(crate) rows: Vec<CellRow>,
    pub(crate) start: u32,
    pub(crate) floor_reason: ScrollbackHistoryFloor,
}

/// A refused page: the guard, plus what came back, so the dropped wave is
/// attributable from one log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageRefusal {
    pub(crate) guard: ChunkGuard,
    pub(crate) grid_epoch: String,
    pub(crate) cols: u32,
    pub(crate) scrollback_total: u64,
    pub(crate) start_row: u64,
    pub(crate) end_row: u64,
    pub(crate) rows: usize,
}

/// Admit a page against the demand it answers, or name the guard refusing it.
///
/// A failing page is refused whole, never approximated: the painted set would
/// otherwise claim coverage it does not have. The epoch is judged first so an
/// incident names the renumber rather than the symptom it produced.
pub(crate) fn validate_page(
    page: ScrollbackPage,
    demand: &Demand,
) -> Result<ValidatedPage, PageRefusal> {
    let Some(guard) = page_guard(&page, demand) else {
        let start = u32::try_from(page.start_row).unwrap_or(demand.bounds.end);
        return Ok(ValidatedPage {
            rows: page.rows,
            start,
            floor_reason: page.history_floor,
        });
    };
    Err(PageRefusal {
        guard,
        grid_epoch: page.grid_epoch,
        cols: page.cols,
        scrollback_total: page.scrollback_total,
        start_row: page.start_row,
        end_row: page.end_row,
        rows: page.rows.len(),
    })
}

/// The first guard a page fails, in v2's order, or `None` when it is admissible.
fn page_guard(page: &ScrollbackPage, demand: &Demand) -> Option<ChunkGuard> {
    let end = u64::from(demand.bounds.end);
    if page.grid_epoch != demand.grid_epoch {
        return Some(ChunkGuard::Epoch);
    }
    if page.cols != demand.cols {
        return Some(ChunkGuard::Cols);
    }
    if page.scrollback_total < demand.minimum_total || page.scrollback_total < end {
        return Some(ChunkGuard::Total);
    }
    if page.start_row > end {
        return Some(ChunkGuard::StartRow);
    }
    if page.end_row != end {
        return Some(ChunkGuard::EndRow);
    }
    // Both edges are inside `[0, demand.end]` now, so they fit a row index.
    if page.rows.len() as u64 != page.end_row - page.start_row {
        return Some(ChunkGuard::RowCount);
    }
    let named_in_order = page
        .rows
        .iter()
        .zip(page.start_row..)
        .all(|(row, index)| u64::from(row.index) == index);
    (!named_in_order).then_some(ChunkGuard::RowIndex)
}

/// The retained floor an admitted page PROVED, or `None` when it served what
/// was asked. A page starting newer than the demand is how a floor is learned:
/// the worker's ring dropped the prefix, and the floor only ever rises.
pub(crate) fn proven_floor(page: &ValidatedPage, demand: &DemandBounds, held: u32) -> Option<u32> {
    (page.start > demand.start).then(|| held.max(page.start))
}
