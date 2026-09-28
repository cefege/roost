//! Running one wave: its read and one retry, admitting the page, noting the
//! retained floor, splicing newest chunk first with a frame yield between
//! chunks, and the settle that hands back to `backfill::demand`. Ports
//! `isCurrent`/`fetchPage`/`runDemand`/`splicePage` of
//! `apps/web/src/renderer/scrollbackBackfill.ts`; a file split, not a type
//! split — the state lives on `ScrollbackBackfill`.

use crate::backfill::request::{
    Demand, DemandKind, ScrollbackPage, ScrollbackPageRequest, ValidatedPage, proven_floor,
    validate_page,
};
use crate::backfill::{BACKFILL_RETRY_MS, BackfillAction, BackfillHost, ScrollbackBackfill};

/// Rows one splice inserts at most, newest first.
const BACKFILL_SPLICE_ROWS: u32 = 250;

/// The wave in flight and the await point it is parked on.
#[derive(Debug)]
pub(crate) struct ActiveWave {
    pub(crate) demand: Demand,
    fetch_retried: bool,
    phase: WavePhase,
}

/// v2's await points: the read, the retry sleep, the frame between chunks.
#[derive(Debug)]
enum WavePhase {
    Fetching,
    RetryPending,
    Splicing(SpliceCursor),
}

/// An admitted page and the next row of it not yet handed to the host.
#[derive(Debug)]
struct SpliceCursor {
    page: ValidatedPage,
    cursor: usize,
    first_requested_offset: usize,
}

impl ActiveWave {
    pub(crate) fn new(demand: Demand) -> Self {
        Self {
            demand,
            fetch_retried: false,
            phase: WavePhase::Fetching,
        }
    }

    /// The answer a find wave that ended without its page owes its reveal.
    pub(crate) fn retired_find(self) -> Option<BackfillAction> {
        (self.demand.kind == DemandKind::Find).then_some(BackfillAction::FindSettled {
            row: self.demand.bounds.focus,
            painted: false,
        })
    }
}

impl ScrollbackBackfill {
    /// The page for `wave` arrived: admit it, note the floor it proves, splice.
    pub fn on_page(
        &mut self,
        wave: u64,
        page: ScrollbackPage,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let Some(demand) = self.parked_demand(wave, |phase| matches!(phase, WavePhase::Fetching))
        else {
            return Vec::new();
        };
        if !self.wave_is_current(&demand, host) {
            return self.settle_wave(false, host);
        }
        let page = match validate_page(page, &demand) {
            Ok(page) => page,
            Err(refusal) => {
                tracing::info!(
                    target: "scrollback",
                    sid = %self.session_id,
                    guard = refusal.guard.as_str(),
                    requested_start = demand.bounds.start,
                    requested_end = demand.bounds.end,
                    response_epoch = %refusal.grid_epoch,
                    response_cols = refusal.cols,
                    response_total = refusal.scrollback_total,
                    start_row = refusal.start_row,
                    end_row = refusal.end_row,
                    rows = refusal.rows,
                    "scrollback.backfill_rejected"
                );
                return self.settle_wave(false, host);
            }
        };
        self.note_floor(&page, &demand, host);
        self.begin_splice(page, &demand, host)
    }

    /// The read for `wave` failed: retry once on the cadence, then give up.
    pub fn on_fetch_failed(
        &mut self,
        wave: u64,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let Some(demand) = self.parked_demand(wave, |phase| matches!(phase, WavePhase::Fetching))
        else {
            return Vec::new();
        };
        let retried = self
            .active_wave
            .as_ref()
            .is_some_and(|live| live.fetch_retried);
        if retried || !self.wave_is_current(&demand, host) {
            return self.settle_wave(false, host);
        }
        if let Some(live) = self.active_wave.as_mut() {
            live.fetch_retried = true;
            live.phase = WavePhase::RetryPending;
        }
        tracing::info!(target: "scrollback", sid = %self.session_id, wave, delay_ms = BACKFILL_RETRY_MS, "scrollback.backfill_fetch_retry");
        vec![BackfillAction::ArmFetchRetry {
            wave,
            delay_ms: BACKFILL_RETRY_MS,
        }]
    }

    /// The retry sleep for `wave` elapsed: read again if it is still current.
    pub fn on_fetch_retry_due(
        &mut self,
        wave: u64,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        match self.parked_demand(wave, |phase| matches!(phase, WavePhase::RetryPending)) {
            Some(demand) => self.fetch_for_wave(&demand, host),
            None => Vec::new(),
        }
    }

    /// The frame after a chunk landed: continue the splice if still current.
    pub fn on_animation_frame(
        &mut self,
        wave: u64,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let Some(demand) =
            self.parked_demand(wave, |phase| matches!(phase, WavePhase::Splicing(_)))
        else {
            return Vec::new();
        };
        if !self.wave_is_current(&demand, host) {
            return self.settle_wave(false, host);
        }
        self.splice_next_chunk(&demand, host)
    }

    /// Whether the wave still answers for the pane as it stands (v2 `isCurrent`).
    pub(crate) fn wave_is_current(&self, demand: &Demand, host: &dyn BackfillHost) -> bool {
        let owns_pager = self
            .active_wave
            .as_ref()
            .is_some_and(|live| live.demand.generation == demand.generation);
        if self.disposed || !self.active || !owns_pager || demand.generation != self.generation {
            return false;
        }
        host.backfill_anchor().is_some_and(|anchor| {
            anchor.grid_epoch == demand.grid_epoch
                && anchor.cols == demand.cols
                && anchor.total >= demand.minimum_total
                && anchor.total >= u64::from(demand.bounds.end)
        })
    }

    /// Issue the wave's read, unless it stopped being current first.
    pub(crate) fn fetch_for_wave(
        &mut self,
        demand: &Demand,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        if !self.wave_is_current(demand, host) {
            return self.settle_wave(false, host);
        }
        if let Some(live) = self.active_wave.as_mut() {
            live.phase = WavePhase::Fetching;
        }
        self.requests += 1;
        vec![BackfillAction::Fetch {
            wave: demand.generation,
            request: ScrollbackPageRequest::for_demand(&self.session_id, demand),
        }]
    }

    /// The active wave's demand, when `wave` names it and it is parked where
    /// the callback resumes; a late answer for a retired wave is dropped.
    fn parked_demand(&self, wave: u64, parked: impl Fn(&WavePhase) -> bool) -> Option<Demand> {
        self.active_wave
            .as_ref()
            .filter(|live| live.demand.generation == wave && parked(&live.phase))
            .map(|live| live.demand.clone())
    }

    /// A page starting newer than the demand proves the worker's ring dropped
    /// the prefix: its start IS the retained floor, and the pager parks there.
    fn note_floor(&mut self, page: &ValidatedPage, demand: &Demand, host: &mut dyn BackfillHost) {
        let Some(floor) = proven_floor(page, &demand.bounds, self.retained_floor) else {
            return;
        };
        self.retained_floor = floor;
        self.floor_reason = page.floor_reason.clone();
        host.set_history_floor(floor);
        tracing::info!(
            target: "scrollback",
            sid = %self.session_id,
            row = floor,
            reason = self.floor_reason.as_wire(),
            "scrollback.history_floor_proven"
        );
    }

    /// A page reaching older than the demand may splice only onto a prefix the
    /// pane already painted; otherwise those rows belong to a placeholder the
    /// page cannot share. The surviving suffix of a SHORT page always splices.
    fn begin_splice(
        &mut self,
        page: ValidatedPage,
        demand: &Demand,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let first_requested_offset = demand.bounds.start.saturating_sub(page.start) as usize;
        if page.start < demand.bounds.start
            && !host.has_painted_scrollback_range(page.start, demand.bounds.start)
        {
            return self.settle_wave(false, host);
        }
        let cursor = page.rows.len();
        if let Some(live) = self.active_wave.as_mut() {
            live.phase = WavePhase::Splicing(SpliceCursor {
                page,
                cursor,
                first_requested_offset,
            });
        }
        self.splice_next_chunk(demand, host)
    }

    /// Splice the newest unspliced chunk and yield a frame, or settle the wave.
    fn splice_next_chunk(
        &mut self,
        demand: &Demand,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        loop {
            let Some((cursor, first)) = self.splice_cursor() else {
                return Vec::new();
            };
            if cursor <= first {
                let focus = demand.bounds.focus;
                let painted = host.has_painted_scrollback_range(focus, focus.saturating_add(1));
                return self.settle_wave(painted, host);
            }
            if !self.wave_is_current(demand, host) {
                return self.settle_wave(false, host);
            }
            match self.splice_chunk_ending_at(cursor, demand, host) {
                ChunkStep::AlreadyPainted => self.set_splice_cursor(cursor - 1),
                ChunkStep::Inserted { from } => {
                    self.set_splice_cursor(from);
                    return vec![BackfillAction::AwaitAnimationFrame {
                        wave: demand.generation,
                    }];
                }
                ChunkStep::Refused => return self.settle_wave(false, host),
            }
        }
    }

    /// One iteration of v2's splice loop over the rows before `cursor`.
    fn splice_chunk_ending_at(
        &self,
        cursor: usize,
        demand: &Demand,
        host: &mut dyn BackfillHost,
    ) -> ChunkStep {
        let Some(WavePhase::Splicing(splice)) = self.active_wave.as_ref().map(|live| &live.phase)
        else {
            return ChunkStep::Refused;
        };
        let page = &splice.page;
        let newest = page.rows[cursor - 1].index;
        let past_newest = newest.saturating_add(1);
        if host.has_painted_scrollback_range(newest, past_newest) {
            return ChunkStep::AlreadyPainted;
        }
        let Some(gap) = host.missing_scrollback_range(newest) else {
            return ChunkStep::Refused;
        };
        let start = demand
            .bounds
            .start
            .max(page.start)
            .max(gap.start)
            .max(past_newest.saturating_sub(BACKFILL_SPLICE_ROWS));
        let from = (start - page.start) as usize;
        let rows = &page.rows[from..cursor];
        // A refused re-insert of rows that are already painted is benign; only
        // rows still missing afterwards end the wave.
        let inserted = host.insert_history_page(rows, false);
        if !inserted && !host.has_painted_scrollback_range(rows[0].index, past_newest) {
            return ChunkStep::Refused;
        }
        ChunkStep::Inserted { from }
    }

    fn splice_cursor(&self) -> Option<(usize, usize)> {
        match self.active_wave.as_ref().map(|live| &live.phase) {
            Some(WavePhase::Splicing(splice)) => {
                Some((splice.cursor, splice.first_requested_offset))
            }
            _ => None,
        }
    }

    fn set_splice_cursor(&mut self, next: usize) {
        if let Some(WavePhase::Splicing(splice)) =
            self.active_wave.as_mut().map(|live| &mut live.phase)
        {
            splice.cursor = next;
        }
    }

    /// Retire the wave, re-derive the reader's gap, and answer a find reveal.
    fn settle_wave(&mut self, painted: bool, host: &mut dyn BackfillHost) -> Vec<BackfillAction> {
        let Some(wave) = self.active_wave.take() else {
            return Vec::new();
        };
        let demand = wave.demand;
        tracing::debug!(
            target: "scrollback",
            sid = %self.session_id,
            wave = demand.generation,
            kind = demand.kind.as_str(),
            focus = demand.bounds.focus,
            painted,
            "scrollback.wave_settled"
        );
        let mut actions = self.rearm_after_wave(&demand, host);
        if demand.kind == DemandKind::Find {
            actions.push(BackfillAction::FindSettled {
                row: demand.bounds.focus,
                painted,
            });
        }
        actions
    }
}

/// What one splice iteration did.
enum ChunkStep {
    AlreadyPainted,
    Inserted { from: usize },
    Refused,
}
