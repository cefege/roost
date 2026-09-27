//! One scrollback history request, the answer it may be given, the guards that
//! decide whether a page may paint, and the wave that runs one page.
//!
//! A page is admissible only when it is the interval the pager asked for under
//! the grid the pane is numbered as RIGHT NOW; every guard exists because the
//! alternative is a painted row that disagrees with the frame describing it, which
//! is the "torn seam". The retained-floor clamp is `history_backfill`'s.

use roost_client_core::terminal::history_backfill::DemandBounds;
use roost_protocol::cell::CellRow;
use roost_protocol::terminal_search::ScrollbackHistoryFloor;

use crate::backfill::{
    BACKFILL_IDENTICAL_RETRIES, BACKFILL_RETRY_MS, BackfillAction, BackfillHost,
    ScrollbackBackfill,
};
use crate::block_placeholder::SCROLLBACK_BLOCK_ROWS;

/// One history page request: rows ending at `end_row`, under one epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackPageRequest {
    /// The session whose history is read.
    pub session_id: String,
    /// The newest row the page may contain, exclusive. A request is named by its
    /// NEWEST row because a page clamped at the retained floor comes back short
    /// at the OLD end, and the reader always knows where "now" is.
    pub end_row: u32,
    /// The most rows the page may carry, one sealed block wide.
    pub max_rows: u32,
    /// The grid numbering the page must be addressed to.
    pub grid_epoch: String,
}

impl ScrollbackPageRequest {
    /// The request one demand becomes, excluding the session it belongs to.
    pub fn for_demand(session_id: &str, demand: &DemandBounds, grid_epoch: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            end_row: demand.end,
            max_rows: demand.end - demand.start,
            grid_epoch: grid_epoch.to_string(),
        }
    }

    /// The oldest row this request names.
    pub fn start_row(&self) -> u32 {
        self.end_row.saturating_sub(self.max_rows)
    }
}

/// What raised a demand: a reader gesture, or a find match's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemandKind {
    /// A reader scrolled and the window exposed a gap.
    Scroll,
    /// Find asked for one match's row to be painted.
    Find,
}

impl DemandKind {
    /// The name an incident log reads.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scroll => "scroll",
            Self::Find => "find",
        }
    }
}

/// One demand: the absolute rows to fetch, and the fence the answer must match.
///
/// The bounds are pure arithmetic from `roost_client_core`; the fence is what
/// makes an answer attributable to the wave that asked for it. A page is judged
/// against BOTH, because the right rows under the wrong grid are still wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demand {
    /// The half-open absolute row range, plus the row the wave owes.
    pub bounds: DemandBounds,
    /// Which pager generation raised it; a newer one has already superseded it.
    pub generation: u32,
    /// Whether a reader gesture or find raised it.
    pub kind: DemandKind,
    /// The grid numbering the answer must carry.
    pub grid_epoch: String,
    /// The columns the answer must carry.
    pub cols: u32,
    /// The history total the frame reported when the demand was derived.
    pub minimum_total: u64,
}

impl Demand {}

/// One history page, as the worker answered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackPage {
    /// The rows, naming the requested interval exactly and in order.
    pub rows: Vec<CellRow>,
    /// The first row, inclusive.
    pub start_row: u32,
    /// The row after the last one.
    pub end_row: u32,
    /// The columns the rows were laid out in.
    pub cols: u32,
    /// The total retained scrollback lines the worker holds now.
    pub scrollback_total: u64,
    /// The grid numbering the rows belong to.
    pub grid_epoch: String,
    /// Which retention floor a SHORT page hit, and why.
    pub history_floor: ScrollbackHistoryFloor,
}

/// Why a page was refused. One name per guard, so a dropped wave is attributable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkGuard {
    /// The page was addressed to a grid the pane no longer is.
    Epoch,
    /// The page's columns differ from the frame's.
    Cols,
    /// The page's history total contradicts the frame or demand.
    Total,
    /// The page starts above the demand's end, so it is not this wave's.
    StartRow,
    /// The page does not end exactly where the demand ends.
    EndRow,
    /// The page's row count is not the interval it names.
    RowCount,
    /// A row's index disagrees with its position: the page is out of order.
    RowIndex,
}

impl ChunkGuard {
    /// The name an incident log reads.
    pub const fn as_str(self) -> &'static str {
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

/// A page that passed every guard, plus what it proved about retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPage {
    /// The rows, in ascending order.
    pub rows: Vec<CellRow>,
    /// The first row the page carries.
    pub start: u32,
    /// The row after the last one the page carries.
    pub end: u32,
    /// The floor the worker reported for a short page.
    pub floor_reason: ScrollbackHistoryFloor,
}

impl ValidatedPage {
    /// Whether the page reached further back than the demand asked, which only a
    /// page clamped at the worker's retention floor does.
    pub fn is_short_of(&self, demand: &DemandBounds) -> bool {
        self.start > demand.start
    }

    /// Whether the page covers the demand's focus row, which a wave reports as
    /// its success and a find reveal then scrolls to.
    pub fn covers_focus(&self, demand: &DemandBounds) -> bool {
        demand.focus >= self.start && demand.focus < self.end
    }
}

/// Admit a page against the demand it answers, or name the guard that refused it.
///
/// A page that fails is REFUSED, never approximated: the painted set would then
/// claim coverage it does not have, and every later interval query would answer
/// from that lie. A stale epoch is diagnosed before a row count, so an incident
/// names the renumber that caused the wave rather than the symptom it produced.
pub fn validate_page(
    page: &ScrollbackPage,
    demand: &Demand,
) -> Result<ValidatedPage, ChunkGuard> {
    let rows = &demand.bounds;
    if page.grid_epoch != demand.grid_epoch {
        return Err(ChunkGuard::Epoch);
    }
    if page.start_row > rows.end {
        return Err(ChunkGuard::StartRow);
    }
    if page.end_row != rows.end {
        return Err(ChunkGuard::EndRow);
    }
    if page.cols != demand.cols {
        return Err(ChunkGuard::Cols);
    }
    if page.scrollback_total < demand.minimum_total.max(rows.end as u64) {
        return Err(ChunkGuard::Total);
    }
    if page.rows.len() as u32 != page.end_row - page.start_row {
        return Err(ChunkGuard::RowCount);
    }
    if !rows_name_interval(&page.rows, page.start_row, page.end_row) {
        return Err(ChunkGuard::RowIndex);
    }
    Ok(ValidatedPage {
        rows: page.rows.clone(),
        start: page.start_row,
        end: page.end_row,
        floor_reason: page.history_floor.clone(),
    })
}

/// The retention floor a page PROVED, or none when it served what was asked.
///
/// A page that reached further back than the demand is how a floor is learned:
/// the worker could not serve rows it used to hold, and said so. The floor only
/// rises, and only from an admitted page.
pub fn note_floor(
    page: &ValidatedPage,
    demand: &DemandBounds,
    held: u32,
) -> Option<(u32, ScrollbackHistoryFloor)> {
    if !page.is_short_of(demand) {
        return None;
    }
    Some((page.start.max(held), page.floor_reason.clone()))
}

/// Whether every row names its own position inside the interval.
fn rows_name_interval(rows: &[CellRow], start: u32, end: u32) -> bool {
    rows.len() as u32 == end - start
        && rows
            .iter()
            .enumerate()
            .all(|(offset, row)| row.index == start + offset as u32)
}

/// Running one wave. A file split, not a type split: the state lives on
/// `ScrollbackBackfill`.
impl ScrollbackBackfill {
    /// Absorb one page and splice what it may contribute, newest chunk first.
    ///
    /// The page is admitted or REFUSED whole, and the refusal is reported before
    /// the wave settles, so an incident names the guard rather than the blank gap
    /// the refusal later causes.
    pub fn on_page(
        &mut self,
        page: &ScrollbackPage,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let Some(demand) = self.active_wave.as_ref().map(|wave| wave.demand.clone()) else {
            return Vec::new();
        };
        if !self.wave_is_current(&demand, host) {
            return self.settle_wave(false, host);
        }
        let validated = match validate_page(page, &demand) {
            Ok(validated) => validated,
            Err(guard) => {
                let refused = BackfillAction::PageRefused {
                    guard,
                    requested_start: demand.bounds.start,
                    requested_end: demand.bounds.end,
                    response_start: page.start_row,
                    response_end: page.end_row,
                    rows: page.rows.len(),
                };
                let mut actions = vec![refused];
                actions.extend(self.settle_wave(false, host));
                return actions;
            }
        };
        let floor = note_floor(&validated, &demand, self.retained_floor);
        let mut actions = Vec::new();
        if let Some((row, reason)) = floor {
            self.retained_floor = row;
            self.floor_reason = reason.clone();
            host.set_history_floor(row);
            actions.push(BackfillAction::SetHistoryFloor(row));
        }
        // A page reaching further back than the demand must not splice a prefix
        // the pane has never painted: those rows belong to a placeholder it does
        // not own, and a page may land in exactly one.
        let prefix = validated.start < demand.bounds.start;
        if prefix && !host.has_painted_range(validated.start, demand.bounds.start) {
            actions.extend(self.settle_wave(false, host));
            return actions;
        }
        let first = demand.bounds.start.saturating_sub(validated.start) as usize;
        if let Some(wave) = self.active_wave.as_mut() {
            wave.page = Some(validated);
            wave.cursor = wave.page.as_ref().map_or(0, |page| page.rows.len());
            wave.first_requested_offset = first;
        }
        actions.extend(self.splice_next_chunk(host));
        actions
    }

    /// Absorb the host's answer to a `Splice` action.
    pub fn on_spliced(&mut self, inserted: bool, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        if !inserted {
            return self.settle_wave(false, host);
        }
        self.splice_next_chunk(host)
    }
    /// Splice the next chunk of the wave's page, or settle the wave.
    fn splice_next_chunk(&mut self, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        loop {
            let Some(wave) = self.active_wave.as_ref() else {
                return Vec::new();
            };
            let Some(page) = wave.page.clone() else {
                return Vec::new();
            };
            if !self.wave_is_current(&wave.demand, host) {
                return self.settle_wave(false, host);
            }
            if wave.cursor <= wave.first_requested_offset {
                return self.settle_wave(true, host);
            }
            let newest = page.rows[wave.cursor - 1].index;
            if host.has_painted_range(newest, newest + 1) {
                if let Some(live) = self.active_wave.as_mut() {
                    live.cursor -= 1;
                }
                continue;
            }
            let Some(gap) = host.missing_range(newest) else {
                return self.settle_wave(false, host);
            };
            let start = page
                .start
                .max(wave.demand.bounds.start)
                .max(gap.start)
                .max(newest + 1 - SCROLLBACK_BLOCK_ROWS);
            let from = (start - page.start) as usize;
            let rows = page.rows[from..wave.cursor].to_vec();
            if let Some(live) = self.active_wave.as_mut() {
                live.cursor = from;
            }
            // The host inserts and answers with `on_spliced`: the seam a frame
            // yield goes through, so a gesture raised here supersedes this wave.
            return vec![BackfillAction::Splice(rows)];
        }
    }

    /// Retire the wave, report its focus row, and re-derive.
    fn settle_wave(&mut self, painted: bool, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        let Some(wave) = self.active_wave.take() else {
            return Vec::new();
        };
        let focus = wave.demand.bounds.focus;
        let painted = painted && host.has_painted_range(focus, focus + 1);
        let mut actions = vec![BackfillAction::WaveSettled {
            generation: wave.demand.generation,
            focus_row: focus,
            painted,
        }];
        actions.extend(self.rearm_after_wave(&wave.demand, host));
        actions
    }

    /// Re-derive the reader's gap after a wave, bounded.
    ///
    /// A refused splice is a benign path and the reader keeps reading while a page
    /// lands, so every settle re-derives rather than waiting for the next gesture.
    /// An UNCHANGED derivation relaunches a bounded number of times and then falls
    /// back to the retry cadence: otherwise a gap nothing can close is a hot loop,
    /// and a reader who parked is owed its rows forever.
    fn rearm_after_wave(
        &mut self,
        settled: &Demand,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        self.scroll_demand_owed = false;
        let Some(next) = self.live_scroll_demand(host) else {
            self.identical_retries = 0;
            return Vec::new();
        };
        let same = next == settled.bounds;
        if !same {
            self.identical_retries = 0;
        }
        if same && self.identical_retries >= BACKFILL_IDENTICAL_RETRIES {
            return vec![BackfillAction::DemandRetryDeferred {
                focus: next.focus,
                retries: self.identical_retries,
                delay_ms: BACKFILL_RETRY_MS,
            }];
        }
        if same {
            self.identical_retries += 1;
        }
        let rearmed = BackfillAction::DemandRearmed {
            focus: next.focus,
            identical: same,
        };
        let mut actions = vec![rearmed];
        actions.extend(self.raise_demand(DemandKind::Scroll, Some(next), false, host));
        actions
    }
}
