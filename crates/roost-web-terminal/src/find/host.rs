//! The whole host conversation pane-local find has with the painted grid: the
//! surface it reads, the work it demands, and the one decision a reveal turns
//! on. `find::hits` owns what the grid is told; this module owns the asking.
//! No DOM and no RPC live here, so the reveal ORDER and the epoch fence are
//! testable natively. `find::controller` drives it; `find::renderer_host` is
//! the production host. Ports the renderer/backfill seam of
//! `apps/web/src/renderer/terminalFindController.ts`.

use crate::find::FindRequest;
use crate::find::hits::{ActiveHit, HitRows};
use crate::presentation::BackfillAnchor;

/// The painted surface a find search and reveal talk to.
///
/// A READ of the pane plus the writes a highlight, a reveal and a dismissal
/// perform. Everything that is not a fact about the grid (the request, the
/// debounce, the row pull) is a `FindCommand` instead.
pub trait FindHost {
    /// The pane's absolute range, or `None` while no authoritative frame is
    /// applied. `None` means there is no painted grid to reveal into yet.
    fn anchor(&self) -> Option<BackfillAnchor>;
    /// The grid numbering the pane is displaying, empty before the first frame.
    /// A search pins it, and a match found under any other numbering is not a
    /// match in this pane. Derived from `anchor` so the two cannot disagree.
    fn pane_epoch(&self) -> String {
        self.anchor()
            .map(|anchor| anchor.grid_epoch)
            .unwrap_or_default()
    }
    /// Replace what the grid highlights. Called on every search conclusion, on
    /// every step, and on clear, because a cleared bar is a state the painter has
    /// to be told about rather than an absent one.
    fn publish_hits(&mut self, hits: HitRows, active: Option<ActiveHit>);
    /// Bring one history row into view, parking the reader on it. Only called
    /// after the row was reported painted.
    fn reveal_row(&mut self, row: u32);
    /// The find bar was dismissed: end the find reading interval WITHOUT moving
    /// the view, so the park becomes an ordinary scroll park.
    fn end_find_read(&mut self);
}

/// One thing the find controller demands of its host.
///
/// Commands are returned rather than performed so the ORDER is the tested
/// property: a pull is issued before the reveal that waits on it, and a debounce
/// that is replaced is cancelled before its replacement is armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindCommand {
    /// Issue one page of the bounded scrollback-search chain; answer with
    /// `TerminalFind::on_page` or `TerminalFind::on_search_error` under the
    /// request's `search_id`.
    Search(FindRequest),
    /// Abort the search named here and ask the coordinator to cancel it; its
    /// late answers may not publish.
    CancelSearch {
        /// The correlation id of the search to stop.
        search_id: String,
    },
    /// Make one history row painted (the backfill's `ensure_row_painted`), then
    /// answer `TerminalFind::on_row_painted(reveal, …)` exactly once: `true` at
    /// once when the row already is painted, `false` when the pull is refused or
    /// evicted. A host with no backfill drops it, which abandons that reveal.
    EnsureRowPainted {
        /// The absolute row the reveal is waiting on.
        row: u32,
        /// The reveal the answer settles.
        reveal: u64,
    },
    /// Fire `TerminalFind::on_debounce` once the clock reaches this instant.
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
    /// The row is on the live viewport, which needs no scrollback jump.
    Viewport,
    /// The row is history: it must be painted before the reader is scrolled to it.
    EnsurePainted {
        /// The absolute row the pull is for.
        row: u32,
    },
}

/// What one match's reveal demands, judged against the pane's anchor.
///
/// The epoch is checked FIRST and separately from coverage, because the two
/// failures are not the same: a stale match must be re-searched, while a history
/// row is a pull away from being revealed. Every history row waits on actual
/// painted coverage — an interior or tail gap included — so coverage is never
/// inferred from the head base here; the pull reports it.
#[must_use]
pub fn reveal_decision(anchor: &BackfillAnchor, row: u32, epoch: &str) -> RevealDecision {
    if anchor.grid_epoch != epoch {
        return RevealDecision::StaleEpoch;
    }
    if u64::from(row) >= anchor.total {
        return RevealDecision::Viewport;
    }
    RevealDecision::EnsurePainted { row }
}
