//! Cell-emission cadence: v2 `apps/worker/src/session/session-cell-scheduler.ts`
//! (leading emit + 16 ms trailing coalesce, identity-fenced per stream
//! generation, input-echo promotion) and `session-manager.ts` `markInputSensitive`.
//! `session::emit` schedules from every ingested chunk; `runtime::cell_cadence`
//! owns the timer that runs [`CellEmitter::run_cadence_work`]. The gates it
//! consults are `session::cell_gates` and `session::sync_output`.

use std::time::{Duration, Instant};

use roost_protocol::wire::brand::ChannelId;

use super::cell_gates::CellGate;
use super::emit::{CELL_EMIT_COALESCE_MS, CellEmitter};
use super::types::SessionRecord;

/// Every chunk that lands inside this window after a keystroke's write emits at
/// once; a local shell echoes within a few ms, and an echo later than two
/// coalesce windows is not distinguishable from unrelated output.
pub const INPUT_ECHO_WINDOW_MS: i64 = 2 * CELL_EMIT_COALESCE_MS;

/// The trailing coalesce window, as the cadence's clock spells it.
pub const CELL_EMIT_COALESCE: Duration = Duration::from_millis(CELL_EMIT_COALESCE_MS as u64);

/// One queued emission, fenced to the stream generation it was armed for (v2
/// `CellEmissionSchedule`). `cooldown_until: None` is a queued leading emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CellEmissionSchedule {
    pub(crate) stream_id: String,
    pub(crate) cooldown_until: Option<Instant>,
}

/// What the cadence owes right now, and when it next must look.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CadenceWork {
    /// Channels with a due leading/trailing emit, an owed baseline, or an
    /// expired synchronized-output ceiling. Sorted.
    pub channels: Vec<ChannelId>,
    /// The raw-metadata compatibility lane owes a dispatch.
    pub raw_due: bool,
    /// The semantic metadata lane owes a flush.
    pub metadata_due: bool,
    /// The earliest future deadline, if any.
    pub next_deadline: Option<Instant>,
}

impl CellEmitter {
    pub fn note_dirty(&mut self, channel_id: ChannelId) {
        self.dirty.insert(channel_id);
    }

    pub fn is_dirty(&self, channel_id: ChannelId) -> bool {
        self.dirty.contains(&channel_id)
    }

    pub fn clear_dirty(&mut self, channel_id: ChannelId) {
        self.dirty.remove(&channel_id);
    }

    /// Every channel with work outstanding, sorted.
    pub fn dirty_channels(&self) -> Vec<ChannelId> {
        let mut channels: Vec<ChannelId> = self.dirty.iter().copied().collect();
        channels.sort_unstable();
        channels
    }

    /// v2 `markInputSensitive`: a keystroke was written at `now_ms`, so output
    /// that lands within [`INPUT_ECHO_WINDOW_MS`] leads instead of waiting out
    /// the coalesce window. A later keystroke re-arms the window.
    pub fn note_input_echo(&mut self, channel_id: ChannelId, now_ms: i64) {
        self.input_echo
            .insert(channel_id, now_ms.saturating_add(INPUT_ECHO_WINDOW_MS));
    }

    /// Whether a chunk ingested at `now_ms` falls inside the channel's echo
    /// window. Not consumed: a spinner chunk that lands first must not take
    /// the promotion from the echo that follows it. A lapsed window is dropped.
    pub fn input_echo_armed(&mut self, channel_id: ChannelId, now_ms: i64) -> bool {
        match self.input_echo.get(&channel_id) {
            Some(&armed_until) if now_ms < armed_until => true,
            Some(_) => {
                self.input_echo.remove(&channel_id);
                false
            }
            None => false,
        }
    }

    /// v2 `cancelCellEmission`: retire this channel's queued emission only.
    pub fn cancel_cell_emission(&mut self, channel_id: ChannelId) {
        self.schedules.remove(&channel_id);
    }

    /// Whether an emission is queued for this channel, and whether it is a
    /// trailing cooldown rather than a leading emit.
    pub fn scheduled_emission(&self, channel_id: ChannelId) -> Option<bool> {
        self.schedules
            .get(&channel_id)
            .map(|schedule| schedule.cooldown_until.is_some())
    }

    /// v2 `installTerminalBaseline` for a caller that holds no record: the
    /// cadence builds the forced full on its next pass.
    pub fn request_terminal_baseline(&mut self, channel_id: ChannelId) {
        if self.baselines_owed.insert(channel_id) {
            tracing::debug!(%channel_id, "a forced baseline was queued for the cadence");
        }
        self.wake_cadence();
    }

    /// v2 `scheduleCellEmission`: the rate governor every accepted chunk enters.
    pub fn schedule_cell_emission(
        &mut self,
        record: &mut SessionRecord,
        promote_input_echo: bool,
        now_ms: i64,
        now: Instant,
    ) {
        let channel_id = record.channel_id();
        let Some(stream_id) = self.enabled_stream_id(channel_id) else {
            return;
        };
        let delivery = self.delivery_aggregate(channel_id);
        // Nobody is delivering: the next resumed or registered sink owes a full.
        if delivery.active_sinks == 0 {
            return;
        }
        if !delivery.baseline_ready || delivery.snapshot_pending {
            self.mark_stream_delivery_dirty(channel_id);
            self.note_dirty(channel_id);
            self.note_gate_suppression(channel_id, CellGate::Baseline, now_ms);
            return;
        }
        if self.gate_held(channel_id) {
            self.note_dirty(channel_id);
            self.note_gate_suppression(channel_id, CellGate::ResizeCapture, now_ms);
            return;
        }
        if self.take_repair_latch(channel_id) {
            self.install_terminal_baseline_at(record, now_ms, now);
            return;
        }
        match self.sync_output_action(record, now_ms, now) {
            super::sync_output::SyncOutputAction::Hold => {
                self.note_dirty(channel_id);
                self.note_gate_suppression(channel_id, CellGate::SyncOutput, now_ms);
                // An armed cooldown must not leak a frame through the hold.
                self.cancel_cell_emission(channel_id);
                return;
            }
            super::sync_output::SyncOutputAction::Flush => {
                self.emit_cell_frame_at(record, false, now_ms, now);
                return;
            }
            super::sync_output::SyncOutputAction::Pass => {}
        }
        if let Some((pending_stream, cooling)) = self
            .schedules
            .get(&channel_id)
            .map(|pending| (pending.stream_id.clone(), pending.cooldown_until.is_some()))
        {
            if pending_stream != stream_id {
                self.cancel_cell_emission(channel_id);
            } else {
                self.note_dirty(channel_id);
                // Only a promoted echo may replace an armed cooldown with a
                // fresh leading emit.
                if !promote_input_echo || !cooling {
                    tracing::debug!(
                        target: "terminal_latency",
                        channel_id = %channel_id,
                        promote_input_echo,
                        cooldown_ms_left = self
                            .schedules
                            .get(&channel_id)
                            .and_then(|pending| pending.cooldown_until)
                            .map(|due| due.saturating_duration_since(now).as_millis()),
                        "emit_folded_into_cooldown"
                    );
                    return;
                }
                self.cancel_cell_emission(channel_id);
            }
        }
        tracing::debug!(
            target: "terminal_latency",
            channel_id = %channel_id,
            promote_input_echo,
            "emit_scheduled_leading"
        );
        self.schedules.insert(
            channel_id,
            CellEmissionSchedule {
                stream_id,
                cooldown_until: None,
            },
        );
        self.wake_cadence();
    }

    /// One cadence pass for one channel whose record the caller holds: an owed
    /// forced baseline, an expired synchronized-output ceiling, and a due
    /// leading or trailing emit, each consumed so the pass cannot spin.
    pub fn run_cadence_work(&mut self, record: &mut SessionRecord, now_ms: i64, now: Instant) {
        let channel_id = record.channel_id();
        if self.baselines_owed.remove(&channel_id) {
            self.install_terminal_baseline_at(record, now_ms, now);
        }
        self.fire_sync_output_ceiling(record, now_ms, now);
        let Some(schedule) = self.schedules.get(&channel_id).cloned() else {
            return;
        };
        if schedule.cooldown_until.is_some_and(|due| due > now) {
            return;
        }
        self.schedules.remove(&channel_id);
        if !self.has_live_current_stream(channel_id, &schedule.stream_id) {
            return;
        }
        // A trailing cooldown fires only for work that arrived inside it.
        if schedule.cooldown_until.is_some() && !self.is_dirty(channel_id) {
            return;
        }
        if !self.may_rearm(channel_id, &schedule.stream_id) {
            return;
        }
        tracing::debug!(target: "terminal_latency", channel_id = %channel_id, "cell_frame_emitted");
        self.emit_cell_frame_at(record, false, now_ms, now);
        // The next window is one after the deadline that just came due, not
        // one after the pass that ran it: a deadline derived from the pass's
        // own start carries the runtime's timer overshoot into the period, so
        // the stream runs below the rate its window claims (17.1 ms measured
        // per 16 ms window on the real driver).
        self.arm_trailing_cooldown(
            channel_id,
            &schedule.stream_id,
            now,
            schedule.cooldown_until,
        );
    }

    /// The channel's session is gone: nothing queued for it may run.
    pub fn drop_cadence_work(&mut self, channel_id: ChannelId, now: Instant) {
        self.schedules.remove(&channel_id);
        self.baselines_owed.remove(&channel_id);
        if self
            .sync_output_deadline(channel_id)
            .is_some_and(|due| due <= now)
        {
            self.release_sync_output_hold(channel_id);
        }
    }

    /// What the cadence owes at `now`.
    pub fn cadence_work(&self, now: Instant) -> CadenceWork {
        let mut work = CadenceWork::default();
        let mut next: Option<Instant> = None;
        let mut consider = |deadline: Instant, channel_id: ChannelId, due: &mut Vec<ChannelId>| {
            if deadline <= now {
                due.push(channel_id);
            } else {
                next = Some(next.map_or(deadline, |held| held.min(deadline)));
            }
        };
        work.channels.extend(self.baselines_owed.iter().copied());
        for (channel_id, schedule) in &self.schedules {
            consider(
                schedule.cooldown_until.unwrap_or(now),
                *channel_id,
                &mut work.channels,
            );
        }
        for (channel_id, deadline) in self.sync_output_deadlines() {
            consider(deadline, channel_id, &mut work.channels);
        }
        work.raw_due = self.raw.dispatch_due(now);
        if let Some(wake) = self.raw.next_wake().filter(|wake| *wake > now) {
            next = Some(next.map_or(wake, |held| held.min(wake)));
        }
        work.metadata_due = self.metadata.flush_due();
        work.channels.sort_unstable();
        work.channels.dedup();
        work.next_deadline = next;
        work
    }

    fn enabled_stream_id(&self, channel_id: ChannelId) -> Option<String> {
        self.streams
            .get(&channel_id)
            .filter(|stream| stream.enabled && stream.core_valid)
            .map(|stream| stream.stream_id.clone())
    }

    /// v2 `hasLiveCurrentStream` (the record's presence is the caller's proof).
    fn has_live_current_stream(&self, channel_id: ChannelId, stream_id: &str) -> bool {
        self.enabled_stream_id(channel_id).as_deref() == Some(stream_id)
    }

    /// v2 `mayRearmCellEmission`.
    fn may_rearm(&self, channel_id: ChannelId, stream_id: &str) -> bool {
        if !self.has_live_current_stream(channel_id, stream_id) {
            return false;
        }
        let delivery = self.delivery_aggregate(channel_id);
        if delivery.active_sinks == 0 || !delivery.baseline_ready || delivery.snapshot_pending {
            return false;
        }
        if self.gate_held(channel_id) || self.repair_latched(channel_id) {
            return false;
        }
        self.sync_output_permits_rearm(channel_id)
    }

    /// v2 `armTrailingCooldown`. `fired` is the deadline this pass ran, if it
    /// ran a trailing one: the window grid continues from the deadline that
    /// came due rather than from the pass that ran it, so the runtime's timer
    /// overshoot is paid once instead of every frame.
    fn arm_trailing_cooldown(
        &mut self,
        channel_id: ChannelId,
        stream_id: &str,
        now: Instant,
        fired: Option<Instant>,
    ) {
        if self.schedules.contains_key(&channel_id) || !self.may_rearm(channel_id, stream_id) {
            return;
        }
        let grid = fired.unwrap_or(now);
        self.schedules.insert(
            channel_id,
            CellEmissionSchedule {
                stream_id: stream_id.to_owned(),
                cooldown_until: Some(grid + CELL_EMIT_COALESCE),
            },
        );
    }
}
