//! The absolute rows one scrollback demand wave asks for.
//!
//! `terminal::history` owns the interval arithmetic — which rows are missing and
//! where the reader is sitting — and this module adds the one question those
//! intervals do not answer: how much of a missing interval to fetch, given the
//! retained floor the worker has already proven and the painted head base the
//! DOM can splice on either side of.
//!
//! Ported from `apps/web/src/client/terminal-stream/scrollbackDemandBounds.ts`,
//! which is pure and reads no DOM, no module state and no wire. Every bound
//! here is reproducible from its arguments alone, which is what makes a bound
//! testable without a renderer.

use crate::terminal::history::{HistoryRange, HistoryScrollTarget};

/// Rows one wave fetches — one worker `SB_BLOCK`. The terminal renderer seals
/// its history blocks at the same 250, and the two numbers are pinned together
/// by `roost-web-terminal/tests/scrollback_backfill.rs`; a fetch page that
/// straddles a sealed block is a page the DOM has to split twice for nothing.
pub const BACKFILL_FETCH_ROWS: u32 = 250;

/// Rows above the viewport the trigger window covers, so a demand is raised
/// before the reader reaches the blank rows it asks for.
pub const BACKFILL_AHEAD_ROWS: u32 = 500;

/// Waves an unchanged derivation may relaunch back to back before it falls back
/// to the retry cadence.
pub const BACKFILL_IDENTICAL_RETRIES: u32 = 2;

/// One demand's half-open absolute row range, plus the row it must end painted.
///
/// `focus` is inside `[start, end)` for every bound this module produces, and
/// that is not cosmetic: a wave reports the FOCUS row as its success, so a
/// demand whose focus the page does not contain answers "painted" for a row the
/// reader cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemandBounds {
    /// The row the wave owes: the reader's own focus, or a find match's row.
    pub focus: u32,
    /// The oldest row the wave may ask for.
    pub start: u32,
    /// The newest row the wave may ask for, exclusive.
    pub end: u32,
}

impl DemandBounds {
    /// The rows the wave asks for, as the interval a history request is named
    /// in. A demand is never empty — that is what `Option` is for.
    pub const fn range(&self) -> HistoryRange {
        HistoryRange {
            start: self.start,
            end: self.end,
        }
    }
}

/// The page the reader's exposed gap demands, or `None` when the pager owes
/// nothing.
///
/// It ends at the newest missing row in the window and extends OLDER, pre-paying
/// what the reader scrolls toward; with under a page older than that edge it
/// extends newer instead. The retained floor clamps the older edge, which is the
/// only thing standing between a reader parked at the top of a trimmed history
/// and an endless re-request of rows the keeper no longer has.
pub fn scroll_demand_bounds(
    target: &HistoryScrollTarget,
    retained_floor: u32,
    painted_base: u32,
) -> Option<DemandBounds> {
    // THE RETAINED-FLOOR INVARIANT, enforced here and nowhere else. `retained_floor`
    // is the oldest row a worker has PROVEN it still holds: a page that came back
    // short. Every request a pager issues is derived through this line, so no
    // request can name a row below a floor the worker has already named itself.
    //
    // A request that ignored it would not merely waste a round trip. The worker
    // answers such a request short, the short answer names the same floor, and
    // the next derivation asks again — a loop that repaints nothing and never
    // terminates, over history that is gone.
    let lower = target.missing.start.max(retained_floor);
    // A page lands in exactly ONE placeholder: below the painted base the
    // renderer splices the head spacer, above it the single gap element covering
    // the page, and no insert spans both. Above the base a page always lies
    // inside one gap — a painted row can never sit inside a missing interval —
    // so the base is the only boundary, and the side holding the newest missing
    // row the window exposed wins: the reader's own rows paint on this wave.
    let exposed = lower.max(target.in_window.end.saturating_sub(1));
    let above_base = exposed >= painted_base;
    let floor_row = if above_base {
        lower.max(painted_base)
    } else {
        lower
    };
    let ceil_row = if above_base {
        target.missing.end
    } else {
        target.missing.end.min(painted_base)
    };
    let end = ceil_row.min(target.in_window.end.max(floor_row + BACKFILL_FETCH_ROWS));
    let start = floor_row.max(end.saturating_sub(BACKFILL_FETCH_ROWS));
    (start < end).then_some(DemandBounds {
        focus: start.max(target.focus_row),
        start,
        end,
    })
}

/// The page a find match demands, or `None` when the pager owes nothing.
///
/// A match needs the context NEWER than itself, so a find page advances forward
/// from its focus row; a top-visible focus still fills the bounded head page.
/// The retained floor clamps the older edge exactly as it does a scroll demand.
pub fn find_demand_bounds(
    gap: HistoryRange,
    focus: u32,
    retained_floor: u32,
    painted_base: u32,
) -> Option<DemandBounds> {
    // THE RETAINED-FLOOR INVARIANT, second enforcement point. A match below the
    // floor is a match against rows the worker has dropped: it cannot be
    // fetched, and a page asking for it comes back short with the same floor it
    // already proved. See `scroll_demand_bounds` for why that is a loop.
    let lower = gap.start.max(retained_floor);
    // The same one-placeholder rule, decided by the MATCH row itself: clamping
    // the page's end to the base instead would paint a page the focus row is not
    // in, and the wave reports the focus row — find would jump to a blank.
    let above_base = focus >= painted_base;
    let floor_row = if above_base {
        lower.max(painted_base)
    } else {
        lower
    };
    let ceil_row = if above_base {
        gap.end
    } else {
        gap.end.min(painted_base)
    };
    let start = if focus.saturating_sub(floor_row) < BACKFILL_FETCH_ROWS {
        floor_row
    } else {
        focus
    };
    let end = ceil_row.min(start + BACKFILL_FETCH_ROWS);
    (start < end).then_some(DemandBounds { focus, start, end })
}

/// Whether a demand names nothing at or below a proven retained floor.
///
/// The assertion the pager makes about its own output, and the one a test that
/// cannot see the DOM leans on: a controller that has proven a floor must be
/// able to prove, from the requests alone, that it stopped asking for the rows
/// underneath it.
#[must_use]
pub fn demand_is_above_floor(bounds: &DemandBounds, retained_floor: u32) -> bool {
    bounds.start >= retained_floor
}
