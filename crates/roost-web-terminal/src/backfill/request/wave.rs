//! Running one wave: absorbing one page, splicing it newest chunk first, and
//! the settle that re-derives the reader's gap instead of waiting for a gesture.
//!
//! A file split, not a type split: the state lives on `ScrollbackBackfill`,
//! which `backfill` owns. `request` decides whether a page may paint; this
//! decides what the pager does with a page it admitted, and what it owes when
//! the wave ends short.

use roost_client_core::terminal::history_backfill::BACKFILL_IDENTICAL_RETRIES;

use crate::backfill::{BACKFILL_RETRY_MS, BackfillAction, BackfillHost, ScrollbackBackfill};
use crate::backfill::request::{Demand, DemandKind, ScrollbackPage, note_floor, validate_page};
use crate::block_placeholder::SCROLLBACK_BLOCK_ROWS;

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
        let floor = note_floor(&validated, &demand.bounds, self.retained_floor);
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
    pub fn on_spliced(
        &mut self,
        inserted: bool,
        host: &mut dyn BackfillHost,
    ) -> Vec<BackfillAction> {
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
