//! Raising, coalescing and re-arming scrollback demands: the `raiseDemand`,
//! `liveScrollDemand`, `rearmAfterWave` and `deferRearm` half of
//! `apps/web/src/renderer/scrollbackBackfill.ts`. A file split, not a type
//! split: the state lives on `ScrollbackBackfill`; `backfill::wave` runs the
//! wave a demand raises and calls back here when it settles.

use roost_client_core::terminal::history_backfill::{
    BACKFILL_AHEAD_ROWS, BACKFILL_IDENTICAL_RETRIES, DemandBounds, scroll_demand_bounds,
};

use crate::backfill::request::{Demand, DemandKind};
use crate::backfill::wave::ActiveWave;
use crate::backfill::{BACKFILL_RETRY_MS, BackfillAction, BackfillHost, ScrollbackBackfill};

/// Whether a raised demand is now owed an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RaiseOutcome {
    /// No wave: nothing to page, the pane is inactive, or no anchor exists.
    Refused,
    /// A wave in flight carries it, new or joined.
    Pending,
}

/// The one deferred re-arm a spent identical-retry budget may hold.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeferredRearm {
    pub(crate) timer: u64,
    /// The demand that was deferred, named by the wake line when the live
    /// derivation has since gone away.
    pub(crate) bounds: DemandBounds,
}

impl ScrollbackBackfill {
    /// A deferred re-arm came due: re-derive the reader's gap and page it.
    pub fn on_deferred_rearm_due(
        &mut self,
        timer: u64,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        let Some(deferred) = self
            .deferred_rearm
            .filter(|deferred| deferred.timer == timer)
        else {
            return Vec::new();
        };
        self.deferred_rearm = None;
        let live = self.live_scroll_demand(host);
        let named = live.unwrap_or(deferred.bounds);
        tracing::info!(
            target: "scrollback",
            sid = %self.session_id,
            focus = named.focus,
            start = named.start,
            end = named.end,
            armed = live.is_some(),
            "scrollback.demand_retry_woke"
        );
        self.raise_demand(DemandKind::Scroll, live, false, host).1
    }

    /// The page the reader's CURRENT scroll position demands, or `None` when
    /// the pager owes nothing: gesture, settle and deferred re-arm derive here.
    pub(crate) fn live_scroll_demand(&self, host: &dyn BackfillHost) -> Option<DemandBounds> {
        if self.disposed || !self.active || host.follows_bottom() {
            return None;
        }
        let target = host.missing_scrollback_range_at_scroll(BACKFILL_AHEAD_ROWS);
        let anchor = host.backfill_anchor();
        scroll_demand_bounds(&target?, self.retained_floor, anchor?.sb_base)
    }

    /// Raise one demand, or join the wave already in flight. Depth stays ONE
    /// wave: a scroll never orphans a page the worker already read and the wire
    /// already carried, because the settle re-derives it.
    pub(crate) fn raise_demand(
        &mut self,
        kind: DemandKind,
        bounds: Option<DemandBounds>,
        reader_intent: bool,
        host: &mut dyn BackfillHost,
    ) -> (RaiseOutcome, Vec<BackfillAction>) {
        let Some(bounds) = bounds.filter(|_| self.active) else {
            return (RaiseOutcome::Refused, Vec::new());
        };
        // Only the owed edge of a gesture reports: a fling raises ~60 scroll
        // events a second and the coalesce line names one wave.
        if reader_intent && !self.scroll_demand_owed {
            self.scroll_demand_owed = true;
            if let Some(wave) = &self.active_wave {
                let existing = &wave.demand;
                tracing::info!(
                    target: "scrollback",
                    sid = %self.session_id,
                    focus = existing.bounds.focus,
                    start = existing.bounds.start,
                    end = existing.bounds.end,
                    kind = existing.kind.as_str(),
                    "scrollback.demand_coalesced"
                );
            }
        }
        if let Some(wave) = &self.active_wave
            && (kind == DemandKind::Scroll
                || (wave.demand.kind == kind && wave.demand.bounds.focus == bounds.focus))
        {
            return (RaiseOutcome::Pending, Vec::new());
        }
        let Some(anchor) = host.backfill_anchor() else {
            return (RaiseOutcome::Refused, Vec::new());
        };
        self.generation += 1;
        let demand = Demand {
            bounds,
            generation: self.generation,
            kind,
            grid_epoch: anchor.grid_epoch,
            cols: anchor.cols,
            minimum_total: anchor.total,
        };
        tracing::debug!(
            target: "scrollback",
            sid = %self.session_id,
            wave = demand.generation,
            kind = kind.as_str(),
            focus = bounds.focus,
            start = bounds.start,
            end = bounds.end,
            "scrollback.wave_raised"
        );
        let preempted = self.active_wave.replace(ActiveWave::new(demand.clone()));
        let mut actions: Vec<BackfillAction> = preempted
            .into_iter()
            .filter_map(ActiveWave::retired_find)
            .collect();
        actions.extend(self.fetch_for_wave(&demand, host));
        (RaiseOutcome::Pending, actions)
    }

    /// Every settle re-derives the live demand rather than waiting for the next
    /// scroll event, because a wave ends short on benign paths while the reader
    /// keeps reading. An unchanged derivation relaunches a bounded number of
    /// times, then falls back to the retry cadence.
    pub(crate) fn rearm_after_wave(
        &mut self,
        settled: &Demand,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
        self.scroll_demand_owed = false;
        let Some(next) = self.live_scroll_demand(host) else {
            self.reset_retry_budget();
            return Vec::new();
        };
        let same = next == settled.bounds;
        if !same {
            self.reset_retry_budget();
        }
        if same && self.identical_retries >= BACKFILL_IDENTICAL_RETRIES {
            return self.defer_rearm(next);
        }
        if same {
            self.identical_retries += 1;
        }
        tracing::info!(
            target: "scrollback",
            sid = %self.session_id,
            focus = next.focus,
            start = next.start,
            end = next.end,
            settled_focus = settled.bounds.focus,
            settled_start = settled.bounds.start,
            settled_end = settled.bounds.end,
            identical = same,
            "scrollback.demand_rearmed"
        );
        self.raise_demand(DemandKind::Scroll, Some(next), false, host)
            .1
    }

    /// The identical-retry budget fences a hot loop of back-to-back waves; it is
    /// not permission to leave the reader's own rows blank, so a spent budget
    /// over a live gap derives again one interval later instead.
    fn defer_rearm(&mut self, next: DemandBounds) -> Vec<BackfillAction> {
        if self.deferred_rearm.is_some() {
            return Vec::new();
        }
        tracing::info!(
            target: "scrollback",
            sid = %self.session_id,
            focus = next.focus,
            start = next.start,
            end = next.end,
            retries = self.identical_retries,
            delay_ms = BACKFILL_RETRY_MS,
            "scrollback.demand_retry_deferred"
        );
        self.rearm_timers += 1;
        self.deferred_rearm = Some(DeferredRearm {
            timer: self.rearm_timers,
            bounds: next,
        });
        vec![BackfillAction::ArmDeferredRearm {
            timer: self.rearm_timers,
            delay_ms: BACKFILL_RETRY_MS,
        }]
    }
}
