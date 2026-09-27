//! The whole host conversation pane-local find has with the painted grid: the
//! surface it reads, the work it demands, and the one decision a reveal turns
//! on. `find::hits` owns what the grid is told; this module owns the asking.
//!
//! No DOM and no RPC live here, so the reveal ORDER and the epoch fence are
//! testable natively. `find::controller` drives all of it, and the production
//! implementation is the renderer that owns the pane's DOM.

use crate::find::FindRequest;
use crate::find::hits::{ActiveHit, HitRows};
use crate::presentation::BackfillAnchor;

/// The painted surface a find search and reveal talk to.
///
/// Five questions, and no more: what the grid is numbered, what it has painted,
/// what to highlight, what to scroll to, and when the read is over. Everything a
/// reveal needs that is not a fact about the grid (the request, the debounce) is
/// a `FindCommand` instead, so this trait stays a READ of the pane plus the two
/// writes a highlight and a reveal perform.
pub trait FindHost {
    /// The grid numbering the pane is displaying. A search pins it, and a match
    /// found under any other numbering is not a match in this pane.
    fn pane_epoch(&self) -> String;
    /// The pane's absolute range, or `None` before the first frame has landed.
    /// `None` means there is no painted grid to reveal into yet.
    fn anchor(&self) -> Option<BackfillAnchor>;
    /// Replace what the grid highlights. Called on every search conclusion, on
    /// every step, and on clear, because a cleared bar is a state the painter has
    /// to be told about rather than an absent one.
    fn publish_hits(&mut self, hits: &HitRows, active: Option<ActiveHit>);
    /// Bring one row into view. Only called for a row already painted, so this
    /// scrolls to something the reader can actually see.
    fn reveal_row(&mut self, row: u32);
    /// The search that was reading this pane has ended, however it ended.
    fn end_find_read(&mut self);
}

/// One thing the find controller demands of its host.
///
/// Commands are returned rather than performed so the ORDER is the tested
/// property: a pull is issued before the reveal that waits on it, and a debounce
/// that is replaced is cancelled before its replacement is armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindCommand {
    /// Issue one page of the bounded scrollback-search chain.
    Search(FindRequest),
    /// Abandon the search named here; its late answers may not publish.
    CancelSearch {
        /// The correlation id of the search to stop.
        search_id: String,
    },
    /// Pull one history row into the painted window for a reveal.
    FetchRow {
        /// The absolute row the reveal is waiting on.
        row: u32,
    },
    /// Fire the debounce once the clock reaches this instant.
    ArmDebounce {
        /// The millisecond instant the debounce comes due.
        at_ms: u64,
    },
    /// Drop whatever debounce is armed, without firing it.
    CancelDebounce,
}

/// What a reveal of one match must do first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealDecision {
    /// The pane has renumbered, so the match names a row in a grid that is gone.
    StaleEpoch,
    /// The row is already painted; scroll to it.
    AlreadyVisible,
    /// The row is history nothing has painted, so pull it before revealing.
    FetchFirst {
        /// The absolute row the pull is for.
        row: u32,
    },
}

/// What one match's reveal demands, judged against the pane's anchor.
///
/// The epoch is checked FIRST and separately from coverage, because the two
/// failures are not the same: a stale match must be re-searched, while an
/// unpainted one is a fetch away from being revealed, and conflating them would
/// either discard a live match or fetch a row out of a grid that no longer holds
/// it.
///
/// Coverage is read from the anchor alone, because that is where the painted
/// range lives: `BackfillAnchor::sb_base` is the painted head base and rows from
/// it up to `total` are painted, so a row BELOW it is scrollback the pager has
/// not spliced in yet. A row at or past `total` is not history the pager could
/// ever hold — the worker has already dropped it — so it is not fetched: asking
/// would spend a wave to be refused.
#[must_use]
pub fn reveal_decision(anchor: &BackfillAnchor, row: u32, epoch: &str) -> RevealDecision {
    if anchor.grid_epoch != epoch {
        return RevealDecision::StaleEpoch;
    }
    if row < anchor.sb_base {
        return RevealDecision::FetchFirst { row };
    }
    RevealDecision::AlreadyVisible
}
