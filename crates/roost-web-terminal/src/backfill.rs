//! Demand-paging terminal history: which absolute rows must be fetched to fill
//! the painted window, the request batching, the retained-floor clamp, and when
//! to stop.
//!
//! `CellGridRenderer` stays the only owner of the DOM and of painted history;
//! `roost_client_core::terminal::history_backfill` owns the page geometry. This
//! controller owns the WAVES: exactly one is in flight, a scroll raised mid-wave
//! is coalesced rather than orphaning a page the worker already read, and every
//! settle re-derives the reader's gap instead of waiting for the next gesture.
//!
//! Ported from `apps/web/src/renderer/scrollbackBackfill.ts`. The wire shapes,
//! the page guards and the wave that runs one page are `backfill::request`; the
//! carrier election is `backfill::direct_history`. No clock is read.

pub mod direct_history;
pub mod request;

pub use direct_history::{DirectHistoryOutcome, HistoryCarrier, direct_history_outcome};
pub use request::{
    ChunkGuard, Demand, DemandKind, ScrollbackPage, ScrollbackPageRequest, ValidatedPage,
    validate_page,
};

use roost_client_core::terminal::history::{HistoryRange, HistoryScrollTarget};
use roost_client_core::terminal::history_backfill::{
    BACKFILL_AHEAD_ROWS, BACKFILL_IDENTICAL_RETRIES, DemandBounds, find_demand_bounds,
    scroll_demand_bounds,
};
use roost_protocol::cell::CellRow;
use roost_protocol::terminal_search::ScrollbackHistoryFloor;

use crate::presentation::BackfillAnchor;

/// Cadence of every pager retry: a failed fetch, and a spent identical-retry
/// budget over a live gap. One wave per interval cannot hot-loop.
pub const BACKFILL_RETRY_MS: u64 = 2000;

/// The painted surface a pager splices into.
pub trait BackfillHost {
    /// The pane's grid identity and absolute range, or nothing before the first
    /// frame lands.
    fn anchor(&self) -> Option<BackfillAnchor>;
    /// Whether the reader is riding the live tail, which owes no history.
    fn follows_bottom(&self) -> bool;
    /// Whether every row of a half-open range is painted.
    fn has_painted_range(&self, start: u32, end: u32) -> bool;
    /// The missing interval containing one row.
    fn missing_range(&self, row: u32) -> Option<HistoryRange>;
    /// The missing interval the reader's own position exposes, widened upward.
    fn missing_range_at_scroll(&self, ahead_rows: u32) -> Option<HistoryScrollTarget>;
    /// Tell the renderer which rows the worker has proven it still retains.
    fn set_history_floor(&mut self, row: u32);
}

/// What the host must do for one pager step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillAction {
    /// Fetch one page of history rows.
    Fetch(ScrollbackPageRequest),
    /// Splice these rows into the painted sheet, newest chunk first.
    Splice(Vec<CellRow>),
    /// The rows the worker has now proven it still retains.
    SetHistoryFloor(u32),
    /// A wave finished, and whether its focus row ended painted.
    WaveSettled {
        /// The pager generation that owned the wave.
        generation: u32,
        /// The row the wave owed the reader.
        focus_row: u32,
        /// Whether that row is painted now.
        painted: bool,
    },
    /// A page was refused, and the guard that refused it.
    PageRefused {
        /// The guard that named the mismatch.
        guard: ChunkGuard,
        /// What the wave asked for.
        requested_start: u32,
        /// What the wave asked for.
        requested_end: u32,
        /// What came back.
        response_start: u32,
        /// What came back.
        response_end: u32,
        /// How many rows came back.
        rows: usize,
    },
    /// A gesture arrived while a wave was in flight, so it joined that wave.
    DemandCoalesced {
        /// What raised the wave the gesture joined.
        kind: DemandKind,
    },
    /// A settled wave re-derived the reader's gap and asked again.
    DemandRearmed {
        /// The row the new demand owes.
        focus: u32,
        /// Whether it derived the identical demand the settled wave did.
        identical: bool,
    },
    /// The identical-retry budget is spent, so the next attempt waits an interval.
    DemandRetryDeferred {
        /// The row the deferred demand owes.
        focus: u32,
        /// How many identical waves have already been spent.
        retries: u32,
        /// How long until the next attempt, in milliseconds.
        delay_ms: u64,
    },
    /// A deferred re-arm woke and re-derived the reader's gap.
    DemandRetryWoke {
        /// The row it owes.
        focus: u32,
        /// Whether a live gap armed a wave.
        armed: bool,
    },
}

/// The wave currently in flight, and the page it is splicing.
#[derive(Debug, Clone)]
struct ActiveWave {
    demand: Demand,
    page: Option<ValidatedPage>,
    /// The next row of `page` this wave has not yet handed the host.
    cursor: usize,
    /// The first offset of `page` the demand actually asked for.
    first_requested_offset: usize,
    /// Whether this wave has already spent its one fetch retry.
    fetch_retried: bool,
}

/// Per-pane scrollback pager: one wave at a time, fenced by generation, epoch,
/// columns, total and real painted coverage.
#[derive(Debug)]
pub struct ScrollbackBackfill {
    session_id: String,
    generation: u32,
    active_wave: Option<ActiveWave>,
    disposed: bool,
    frame_epoch: Option<String>,
    frame_cols: u32,
    frame_total: i64,
    /// THE RETAINED FLOOR: the oldest row a worker has PROVEN it still holds.
    /// Every request this pager issues is derived through the clamp in
    /// `history_backfill`, so no request can name a row under it.
    retained_floor: u32,
    floor_reason: ScrollbackHistoryFloor,
    scroll_demand_owed: bool,
    identical_retries: u32,
    deferred_rearm_at: Option<u64>,
    requests: u32,
}

impl ScrollbackBackfill {
    /// A pager for one session that has never fetched a page.
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            generation: 0,
            active_wave: None,
            disposed: false,
            frame_epoch: None,
            frame_cols: 0,
            frame_total: -1,
            retained_floor: 0,
            floor_reason: ScrollbackHistoryFloor::None,
            scroll_demand_owed: false,
            identical_retries: 0,
            deferred_rearm_at: None,
            requests: 0,
        }
    }

    /// The oldest row the worker has proven it still retains, 0 when never hit.
    pub fn retained_floor(&self) -> u32 {
        self.retained_floor
    }
    /// Why that floor is there, straight off the page that established it.
    pub fn floor_reason(&self) -> &ScrollbackHistoryFloor {
        &self.floor_reason
    }
    /// History RPCs this pager has issued.
    pub fn request_count(&self) -> u32 {
        self.requests
    }
    /// Observe a full frame: a renumbered or rewound grid retires every wave.
    ///
    /// A full frame never prefetches. What it does is FENCE: a grid that renumbers
    /// or a total that rewinds invalidates every page in flight, and the retained
    /// floor goes with them.
    pub fn on_full_frame(&mut self, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        let anchor = host.anchor();
        let total = anchor.as_ref().map_or(-1, |a| a.total as i64);
        let rewound = anchor.as_ref().is_some_and(|a| {
            (self.frame_total >= 0 && a.total < self.frame_total as u64)
                || self
                    .active_wave
                    .as_ref()
                    .is_some_and(|wave| a.total < wave.demand.minimum_total)
        });
        let changed = match &anchor {
            None => true,
            Some(a) => {
                a.grid_epoch != self.frame_epoch.clone().unwrap_or_default()
                    || a.cols != self.frame_cols
                    || rewound
            }
        };
        if changed {
            self.suspend();
            self.frame_epoch = anchor.as_ref().map(|a| a.grid_epoch.clone());
            self.frame_cols = anchor.as_ref().map_or(0, |a| a.cols);
            self.frame_total = total;
            return self.clear_floor(host);
        }
        if let Some(a) = &anchor {
            self.frame_total = self.frame_total.max(a.total as i64);
        }
        Vec::new()
    }

    /// A reader gesture: re-arm the budget and page the gap it exposes.
    pub fn on_user_scroll(&mut self, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        // A real gesture re-arms the budget and supersedes a deferred re-arm.
        self.deferred_rearm_at = None;
        self.identical_retries = 0;
        let live = self.live_scroll_demand(host);
        self.raise_demand(DemandKind::Scroll, live, true, host)
    }

    /// Bring one history row into the painted window, for a find reveal. Whether
    /// the row ended painted is reported by `WaveSettled`, which is what a reveal
    /// waits for rather than assuming.
    pub fn ensure_row_painted(
        &mut self,
        row: u32,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        if self.disposed || host.has_painted_range(row, row + 1) {
            return Vec::new();
        }
        let Some(gap) = host.missing_range(row) else {
            return Vec::new();
        };
        let Some(anchor) = host.anchor() else {
            return Vec::new();
        };
        let bounds =
            find_demand_bounds(gap, row, self.retained_floor, anchor.sb_base);
        self.raise_demand(DemandKind::Find, bounds, false, host)
    }

    /// A fetch that never arrived, for whatever reason.
    ///
    /// The wave survives so the next `tick` can spend its ONE retry: a worker that
    /// cannot serve a row must be asked once more on the cadence, and never on a
    /// loop.
    pub fn on_fetch_failed(&mut self, now_ms: u64) {
        let wave = self.active_wave.as_mut();
        if let Some(wave) = wave
            && !wave.fetch_retried
        {
            wave.fetch_retried = true;
            self.deferred_rearm_at = Some(now_ms + BACKFILL_RETRY_MS);
        }
    }

    /// Retire every wave and cancel the deferred re-arm.
    pub fn suspend(&mut self) {
        self.generation += 1;
        self.active_wave = None;
        self.scroll_demand_owed = false;
        self.identical_retries = 0;
        self.deferred_rearm_at = None;
    }

    /// Retire the pager for good.
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.suspend();
    }

    /// Fire a deferred retry whose time has come.
    pub fn tick(&mut self, now_ms: u64, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        let mut actions = Vec::new();
        if self.deferred_rearm_at.is_some_and(|at_ms| now_ms >= at_ms) {
            self.deferred_rearm_at = None;
            let live = self.live_scroll_demand(host);
            let focus = live.map_or(0, |bounds| bounds.focus);
            actions.push(BackfillAction::DemandRetryWoke {
                focus,
                armed: live.is_some(),
            });
            actions.extend(self.raise_demand(DemandKind::Scroll, live, false, host));
        }
        actions
    }

    /// The page the reader's own position demands, or none when the pager owes
    /// nothing: a follower at the tail owes no history, and a pane with no
    /// anchor cannot be derived from at all.
    pub(crate) fn live_scroll_demand(&mut self, host: &dyn BackfillHost) -> Option<DemandBounds> {
        if self.disposed || host.follows_bottom() {
            return None;
        }
        let anchor = host.anchor()?;
        let target = host.missing_range_at_scroll(BACKFILL_AHEAD_ROWS)?;
        scroll_demand_bounds(&target, self.retained_floor, anchor.sb_base)
    }

    /// Whether the wave still answers for the pane as it stands.
    pub(crate) fn wave_is_current(&self, demand: &Demand, host: &dyn BackfillHost) -> bool {
        if self.disposed || demand.generation != self.generation {
            return false;
        }
        let Some(anchor) = host.anchor() else {
            return false;
        };
        anchor.grid_epoch == demand.grid_epoch
            && anchor.cols == demand.cols
            && anchor.total >= demand.minimum_total
            && anchor.total >= demand.bounds.end as u64
    }

    /// Raise one demand, or join the wave already in flight.
    ///
    /// Depth stays ONE wave. A scroll never orphans a page the worker already
    /// read and the wire already carried, because the settle re-derives it.
    pub(crate) fn raise_demand(
        &mut self,
        kind: DemandKind,
        bounds: Option<DemandBounds>,
        reader_gesture: bool,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let Some(bounds) = bounds else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        // Only the owed edge of a gesture reports: a fling raises ~60 scroll
        // events a second and the coalesce line names one wave, not sixty.
        if reader_gesture && !self.scroll_demand_owed {
            self.scroll_demand_owed = true;
            if let Some(wave) = &self.active_wave {
                actions.push(BackfillAction::DemandCoalesced { kind: wave.demand.kind });
            }
        }
        if let Some(wave) = &self.active_wave {
            if kind == DemandKind::Scroll
                || (wave.demand.kind == kind && wave.demand.bounds.focus == bounds.focus)
            {
                return actions.extend(vec![]);
            }
        }
        let Some(anchor) = host.anchor() else {
            return actions;
        };
        self.generation += 1;
        let demand = Demand {
            bounds,
            generation: self.generation,
            kind,
            grid_epoch: anchor.grid_epoch.clone(),
            cols: anchor.cols,
            minimum_total: anchor.total,
        };
        self.active_wave = Some(ActiveWave {
            demand: demand.clone(),
            page: None,
            cursor: 0,
            first_requested_offset: 0,
            fetch_retried: false,
        });
        actions.push(BackfillAction::Fetch(ScrollbackPageRequest::for_demand(&self.session_id, &demand.bounds, &demand.grid_epoch)));
        actions
    }

    /// Forget the proven floor, and tell the renderer the head rows are back.
    pub(crate) fn clear_floor(&mut self, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        self.retained_floor = 0;
        self.floor_reason = ScrollbackHistoryFloor::None;
        host.set_history_floor(0);
        vec![BackfillAction::SetHistoryFloor(0)]
    }
 }
