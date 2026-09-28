//! Demand-pages immutable terminal history at the visible or find-targeted gap.
//! `CellGridRenderer` stays the only DOM/history owner behind `BackfillHost`,
//! `roost_client_core::terminal::history_backfill` owns the page geometry, and
//! this pager fences one wave by generation, epoch, columns, total and painted
//! coverage. Ports `apps/web/src/renderer/scrollbackBackfill.ts`; its RPC, frame
//! yield and timers are `BackfillAction`s the pane performs and answers.

mod demand;
pub mod direct_history;
mod renderer_host;
pub mod request;
mod wave;

pub use direct_history::{
    DirectHistoryFailure, DirectHistoryOutcome, DirectReadError, direct_history_outcome,
    elected_direct_route,
};
pub use request::{ScrollbackPage, ScrollbackPageRequest};

use roost_client_core::terminal::history::{HistoryRange, HistoryScrollTarget};
use roost_client_core::terminal::history_backfill::find_demand_bounds;
use roost_protocol::cell::CellRow;
use roost_protocol::terminal_search::ScrollbackHistoryFloor;

use crate::backfill::demand::{DeferredRearm, RaiseOutcome};
use crate::backfill::request::DemandKind;
use crate::backfill::wave::ActiveWave;
use crate::presentation::BackfillAnchor;

/// Cadence of every pager retry: a failed fetch, and a spent identical-retry
/// budget over a live gap. One wave per interval cannot hot-loop.
pub const BACKFILL_RETRY_MS: u64 = 2000;

/// The renderer surface one pager reads and splices into, v2's
/// `Pick<CellGridRenderer, …>`: the production host is `CellGridRenderer`.
pub trait BackfillHost {
    /// The grid identity and painted head base, or `None` before a frame lands.
    fn backfill_anchor(&self) -> Option<BackfillAnchor>;
    /// Whether the reader rides the live tail inside the follow band.
    fn follows_bottom(&self) -> bool;
    /// Whether every row of a nonempty half-open absolute range is painted.
    fn has_painted_scrollback_range(&self, start: u32, end: u32) -> bool;
    /// The missing interval containing one row, or `None` when it is painted.
    fn missing_scrollback_range(&self, row: u32) -> Option<HistoryRange>;
    /// The missing interval the reader's position exposes, widened older.
    fn missing_scrollback_range_at_scroll(&self, ahead_rows: u32) -> Option<HistoryScrollTarget>;
    /// Record the oldest row the worker proved it retains; 0 clears it.
    fn set_history_floor(&mut self, row: u32);
    /// Splice one contiguous page into the one placeholder it fits, or refuse.
    fn insert_history_page(&mut self, rows: &[CellRow], follow_tail: bool) -> bool;
}

/// Work the pager hands its pane: v2's awaited RPC, frame and timers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillAction {
    /// Issue one history read, then answer `on_page` or `on_fetch_failed`.
    Fetch {
        /// The wave the answer belongs to.
        wave: u64,
        /// The query to send.
        request: ScrollbackPageRequest,
    },
    /// Yield one animation frame, then call `on_animation_frame`.
    AwaitAnimationFrame {
        /// The wave that is splicing.
        wave: u64,
    },
    /// Sleep, then call `on_fetch_retry_due` for the wave's one retry.
    ArmFetchRetry {
        /// The wave whose read failed.
        wave: u64,
        /// How long to wait.
        delay_ms: u64,
    },
    /// Sleep, then call `on_deferred_rearm_due` to re-derive the reader's gap.
    ArmDeferredRearm {
        /// The token the wake must present; a superseded one is ignored.
        timer: u64,
        /// How long to wait.
        delay_ms: u64,
    },
    /// A find demand concluded, and whether its row is painted now.
    FindSettled {
        /// The absolute row the reveal asked for.
        row: u32,
        /// Whether that row is painted.
        painted: bool,
    },
}

/// Per-pane scrollback pager: exactly one wave in flight at a time.
#[derive(Debug)]
pub struct ScrollbackBackfill {
    session_id: String,
    generation: u64,
    active_wave: Option<ActiveWave>,
    disposed: bool,
    active: bool,
    frame_epoch: Option<String>,
    frame_cols: u32,
    frame_total: Option<u64>,
    /// The oldest row a worker has PROVEN it still holds, 0 when never hit.
    /// Every demand derives through the clamp in `history_backfill`, so no
    /// request can name a row under it.
    retained_floor: u32,
    floor_reason: ScrollbackHistoryFloor,
    scroll_demand_owed: bool,
    identical_retries: u32,
    deferred_rearm: Option<DeferredRearm>,
    rearm_timers: u64,
    requests: u64,
}

impl ScrollbackBackfill {
    /// A pager for one session's pane, active until the pane says otherwise.
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            generation: 0,
            active_wave: None,
            disposed: false,
            active: true,
            frame_epoch: None,
            frame_cols: 0,
            frame_total: None,
            retained_floor: 0,
            floor_reason: ScrollbackHistoryFloor::None,
            scroll_demand_owed: false,
            identical_retries: 0,
            deferred_rearm: None,
            rearm_timers: 0,
            requests: 0,
        }
    }

    /// The proven history floor and WHY it is there, or `None` until a page
    /// actually came back short with a named reason.
    pub fn history_floor(&self) -> Option<(u32, &ScrollbackHistoryFloor)> {
        (self.floor_reason != ScrollbackHistoryFloor::None)
            .then_some((self.retained_floor, &self.floor_reason))
    }

    /// History reads this pager has issued, retries included.
    pub fn request_count(&self) -> u64 {
        self.requests
    }

    /// Whether the pane is viewed and the page visible (v2's `active()`): an
    /// inactive pager raises nothing and every wave in flight stops current.
    pub fn set_active(&mut self, active: bool) {
        if self.active != active {
            tracing::debug!(target: "scrollback", sid = %self.session_id, active, "scrollback.pager_active");
            self.active = active;
        }
    }

    /// Observe a full frame. It never prefetches; a renumbered grid or a total
    /// that rewound retires the wave in flight and forgets the proven floor.
    pub fn on_full_frame(&mut self, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        let anchor = host.backfill_anchor();
        let total_rewound = anchor.as_ref().is_some_and(|anchor| {
            self.frame_total.is_some_and(|total| anchor.total < total)
                || self
                    .active_wave
                    .as_ref()
                    .is_some_and(|wave| anchor.total < wave.demand.minimum_total)
        });
        let identity_changed = anchor.as_ref().is_none_or(|anchor| {
            self.frame_epoch.as_deref() != Some(anchor.grid_epoch.as_str())
                || anchor.cols != self.frame_cols
                || total_rewound
        });
        if !identity_changed {
            if let Some(anchor) = anchor {
                self.frame_total = Some(
                    self.frame_total
                        .map_or(anchor.total, |held| held.max(anchor.total)),
                );
            }
            return Vec::new();
        }
        let actions = self.suspend();
        tracing::info!(
            target: "scrollback",
            sid = %self.session_id,
            grid_epoch = anchor.as_ref().map_or("", |anchor| anchor.grid_epoch.as_str()),
            total_rewound,
            "scrollback.grid_identity_changed"
        );
        self.frame_epoch = anchor.as_ref().map(|anchor| anchor.grid_epoch.clone());
        self.frame_cols = anchor.as_ref().map_or(0, |anchor| anchor.cols);
        self.frame_total = anchor.as_ref().map(|anchor| anchor.total);
        self.clear_floor(host);
        actions
    }

    /// A reader gesture: re-arm the retry budget and page the gap it exposes.
    pub fn on_user_scroll(&mut self, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        // A real gesture re-arms the budget and supersedes the deferred re-arm.
        self.reset_retry_budget();
        let live = self.live_scroll_demand(host);
        self.raise_demand(DemandKind::Scroll, live, true, host).1
    }

    /// Bring one history row into the painted window for a find reveal. The
    /// answer is always a `FindSettled` for `row`: at once, or when its wave ends.
    pub fn ensure_row_painted(
        &mut self,
        row: u32,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let settled = |painted| vec![BackfillAction::FindSettled { row, painted }];
        if !self.active {
            return settled(false);
        }
        if host.has_painted_scrollback_range(row, row.saturating_add(1)) {
            return settled(true);
        }
        let (Some(gap), Some(anchor)) =
            (host.missing_scrollback_range(row), host.backfill_anchor())
        else {
            return settled(false);
        };
        let bounds = find_demand_bounds(gap, row, self.retained_floor, anchor.sb_base);
        let (outcome, mut actions) = self.raise_demand(DemandKind::Find, bounds, false, host);
        if outcome == RaiseOutcome::Refused {
            actions.extend(settled(false));
        }
        actions
    }

    /// Retire the wave in flight and cancel the deferred re-arm. A retired find
    /// wave answers `FindSettled { painted: false }`.
    pub fn suspend(&mut self) -> Vec<BackfillAction> {
        self.generation += 1;
        let retired = self.active_wave.take();
        self.scroll_demand_owed = false;
        self.reset_retry_budget();
        tracing::debug!(target: "scrollback", sid = %self.session_id, generation = self.generation, "scrollback.pager_suspended");
        retired
            .into_iter()
            .filter_map(ActiveWave::retired_find)
            .collect()
    }

    /// Retire the pager for good: nothing it armed may act afterwards.
    pub fn dispose(&mut self) -> Vec<BackfillAction> {
        self.disposed = true;
        self.suspend()
    }

    /// Forget the proven floor and tell the renderer the head rows are back.
    fn clear_floor(&mut self, host: &mut dyn BackfillHost) {
        self.retained_floor = 0;
        self.floor_reason = ScrollbackHistoryFloor::None;
        host.set_history_floor(0);
    }

    /// The retry budget and the deferred re-arm it schedules are one state: a
    /// reader gesture, a changed derivation and a suspend all re-arm both.
    fn reset_retry_budget(&mut self) {
        self.identical_retries = 0;
        self.deferred_rearm = None;
    }
}
