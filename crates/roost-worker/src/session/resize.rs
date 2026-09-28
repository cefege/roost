//! Geometry: the in-place resize at the keeper's ordered boundary and the
//! capture that withholds bytes while it is unresolved (the history pin it
//! writes is `session::resize_pin`'s). `session::terminal_txn` is the production
//! caller; `session::core_reprove` settles a lost acknowledgement through the
//! primitives here. Ports the boundary half of
//! `apps/worker/src/session/session-resize-capture.ts` and the keeper resize of
//! `session-terminal-txn.ts`.
//!
//! THE RESIZE IS IN PLACE, AT THE BOUNDARY: held bytes parse at the OLD size,
//! then `TerminalCore::resize` runs on the same core. Rebuilding from a bounded
//! ring on a resize loses evicted bytes and blanks unchanged cells
//! (`docs/FAILURE-INDEX.md`, "A live viewport change rebuilds the terminal
//! core"). A boundary that cannot be proven TRAPS the core instead of guessing,
//! and the capture hands back its emission gate on that path too.

use std::sync::{Mutex, MutexGuard, PoisonError};

use roost_keeper::client_resize::{ResizeOutcome as KeeperResize, ResizeRejectReason};
use roost_protocol::wire::brand::ChannelId;

use super::binding::ChannelDelivery;
use super::ids::mint_uuid;
use super::lifecycle::SessionManager;
use super::query_reply::answer_queries;
use super::resize_pin::{PinInputs, pin_for};
use super::types::SessionRecord;
use crate::browser_commands::Refusal;

/// What a resize did at the keeper's boundary (v2 `applyResizeResultAtBoundary`
/// and the transaction's admission check, as one answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResizeOutcome {
    /// Exactly this sequence and geometry were acknowledged; the held bytes
    /// parsed at the old size and the SAME core was resized.
    Applied { cols: u16, rows: u16 },
    /// Already at this geometry: nothing written, no sequence spent.
    Unchanged,
    /// Never reached the keeper (v2 `admission.written === false`).
    NotWritten { reason: String },
    /// The keeper refused the written sequence; the core keeps its geometry.
    Refused { reason: ResizeRejectReason },
    /// Unprovable: the core was latched fail-closed (v2 `failCore`).
    Trapped { reason: String },
    /// The answer was lost; the capture is STILL OPEN for `recover_lost_ack`.
    Unknown { boundary: OpenBoundary },
    /// The session closed before its boundary settled; the capture was released.
    SessionClosed,
}

/// A written resize whose capture opened at `install_seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenBoundary {
    pub seq: u64,
    /// `head_seq` when the capture opened: the offset of the first held byte.
    pub install_seq: u64,
    pub from: (u16, u16),
    pub to: (u16, u16),
    /// The probe tokenizer's carry when the capture opened (v2 `queryCarry`):
    /// the held bytes are answered from it at the boundary.
    pub query_carry: Vec<u8>,
}

/// The keeper's answer, minus the lost one `resize_channel` hands back open.
enum BoundaryAnswer {
    NotWritten(String),
    Refused(ResizeRejectReason),
    Applied { seq: u64, cols: u16, rows: u16 },
}

impl SessionManager {
    /// Move a live channel's geometry, in place, at the keeper's boundary. The
    /// capture opens under the record lock; the keeper call runs with NO lock
    /// held, so the dispatcher keeps delivering into the capture; the boundary
    /// settles under both locks again. BLOCKING: run it off the async runtime.
    pub fn resize_channel(
        &self,
        channel_id: ChannelId,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, Refusal> {
        let raw = channel_id.as_u32() as u16;
        let Some(entry) = self.sessions.entry(raw) else {
            return Err(Refusal::failed(
                "resize",
                format!("this worker holds no channel {raw} to resize"),
            ));
        };
        let seq = self.channel_resize_seq(raw) + 1;
        let (from, install_seq, query_carry) = {
            let record = lock(&entry);
            let from = (record.terminal_core.cols(), record.terminal_core.rows());
            if from == (cols, rows) {
                return Ok(ResizeOutcome::Unchanged);
            }
            if !lock(&self.ingest).freeze_capture(channel_id, self.clock.now_epoch_ms()) {
                return Ok(ResizeOutcome::NotWritten {
                    reason: format!("channel {raw} already has an unresolved resize in flight"),
                });
            }
            (from, record.head_seq, record.query_carry.clone())
        };
        let boundary = OpenBoundary {
            seq,
            install_seq,
            from,
            to: (cols, rows),
            query_carry,
        };
        tracing::info!(channel_id = raw, seq, ?from, to = ?(cols, rows), install_seq, "a resize boundary opened and its request went to the keeper");
        let answer = match self.keeper.resize_channel(raw, seq, cols, rows) {
            Err(fault) => BoundaryAnswer::NotWritten(fault.reason),
            Ok(KeeperResize::Refused { reason, .. }) => BoundaryAnswer::Refused(reason),
            Ok(KeeperResize::Applied { seq, cols, rows }) => {
                BoundaryAnswer::Applied { seq, cols, rows }
            }
            Ok(KeeperResize::Unknown { reason, .. }) => {
                self.note_applied_resize_seq(raw, seq);
                tracing::warn!(
                    channel_id = raw,
                    seq,
                    ?reason,
                    "a resize answer was lost; its capture stays open for recovery"
                );
                return Ok(ResizeOutcome::Unknown { boundary });
            }
        };
        if !matches!(answer, BoundaryAnswer::NotWritten(_)) {
            self.note_applied_resize_seq(raw, seq);
        }
        Ok(self.settle_boundary(channel_id, &boundary, answer))
    }

    /// Settle an answered boundary under both locks (v2
    /// `applyResizeResultAtBoundary`). A capture that outgrew the window, an
    /// acknowledgement for another sequence or geometry, or a core that did not
    /// take the size all trap the core rather than guess.
    fn settle_boundary(
        &self,
        channel_id: ChannelId,
        boundary: &OpenBoundary,
        answer: BoundaryAnswer,
    ) -> ResizeOutcome {
        let raw = channel_id.as_u32() as u16;
        let Some(entry) = self.sessions.entry(raw) else {
            lock(&self.ingest).close_capture(channel_id);
            tracing::info!(
                channel_id = raw,
                seq = boundary.seq,
                "a resize boundary's session closed before it settled"
            );
            return ResizeOutcome::SessionClosed;
        };
        let (outcome, loss) = {
            let mut record = lock(&entry);
            let delivery = lock(&self.ingest);
            let captured = delivery.close_capture(channel_id);
            if captured.overflowed {
                let reason = "resize boundary output was evicted before alignment";
                return trap_boundary(&record, &*delivery, boundary, reason);
            }
            // Answering the held bytes IS their core write, at the old size.
            let mut carry = boundary.query_carry.clone();
            let core: Option<&mut dyn roost_term::TerminalCore> =
                Some(record.terminal_core.as_mut());
            let replies = answer_queries(&mut carry, core, &captured.bytes).bytes;
            let settled = match answer {
                BoundaryAnswer::NotWritten(reason) => (ResizeOutcome::NotWritten { reason }, None),
                BoundaryAnswer::Refused(reason) => (ResizeOutcome::Refused { reason }, None),
                BoundaryAnswer::Applied { seq, cols, rows } => {
                    if seq != boundary.seq || (cols, rows) != boundary.to {
                        let reason = "keeper acknowledged conflicting resize geometry";
                        return trap_boundary(&record, &*delivery, boundary, reason);
                    }
                    match self.resize_in_place(&mut record, &*delivery, boundary.to) {
                        Ok(loss) => (ResizeOutcome::Applied { cols, rows }, loss),
                        Err(reason) => {
                            return trap_boundary(&record, &*delivery, boundary, &reason);
                        }
                    }
                }
            };
            if let Some(emission) = delivery.stream_emission() {
                emission.forward_query_replies(&record, replies);
            }
            settled
        };
        report_capture_loss(raw, loss);
        tracing::info!(channel_id = raw, seq = boundary.seq, outcome = ?outcome, "a resize boundary settled");
        outcome
    }

    /// Resize the core where it stands and reset its emission epoch (v2
    /// `wtermCore.resize` + `resetEmissionEpoch`): a new grid identity, every
    /// sink owed a baseline, and every viewport row dirty.
    pub(super) fn resize_in_place(
        &self,
        record: &mut SessionRecord,
        delivery: &dyn ChannelDelivery,
        to: (u16, u16),
    ) -> Result<Option<CaptureLoss>, String> {
        let before = CoreCounters::read(record);
        record.terminal_core.resize(to.0, to.1);
        if (record.terminal_core.cols(), record.terminal_core.rows()) != to {
            return Err("terminal core did not retain validated resize geometry".to_owned());
        }
        let after = CoreCounters::read(record);
        let pin = pin_for(PinInputs {
            at_mono_ms: self.clock.mono_ns() / 1_000_000,
            cols: to.0,
            rows: to.1,
            replayed_ring: false,
            ring_evicted: record.scrollback.evicting(),
            prev_dropped: before.discarded,
            prev_total: before.total,
            fresh_discarded: after.discarded,
            fresh_count: after.retained(),
            previous_replay_floor: record
                .sb_origin_pin
                .map_or(0, |previous| previous.replay_floor),
        });
        record.sb_origin_pin = Some(pin);
        let epoch =
            mint_uuid().map_err(|error| format!("a grid epoch could not be minted: {error}"))?;
        let emit = &mut record.cell_emit;
        emit.grid_epoch_base = epoch;
        emit.grid_epoch_revision = 0;
        emit.sent_full = false;
        (emit.cols, emit.rows, emit.alt) = (0, 0, false);
        if let Some(emission) = delivery.stream_emission() {
            emission.reset_delivery(record.channel_id());
        }
        if let Some(row) = (0..to.1).find(|row| !record.terminal_core.is_dirty_row(*row)) {
            return Err(format!("terminal core resize left row {row} clean"));
        }
        Ok((pin.replay_lost_rows > 0).then_some(CaptureLoss {
            floor: pin.sb_dropped,
            rows: pin.replay_lost_rows,
        }))
    }

    /// The highest resize sequence written to the keeper on a channel (v2
    /// `channelResizeSeq`); zero before the first.
    pub fn channel_resize_seq(&self, channel_id: u16) -> u64 {
        lock(&self.resize_seqs)
            .get(&channel_id)
            .copied()
            .unwrap_or(0)
    }

    /// Record the sequence the keeper now holds for a channel: the one a resize
    /// just wrote, or the one an adoption read back — the keeper rejects a
    /// sequence it has already applied, so the next write must be above it.
    pub fn note_applied_resize_seq(&self, channel_id: u16, applied_seq: u64) {
        lock(&self.resize_seqs).insert(channel_id, applied_seq);
    }
}

/// Latch a core whose boundary cannot be proven (v2 `failCore`). The caller has
/// already closed the capture, so its gate is back; the latch is what keeps
/// emission refused and later chunks on the retain-only lane.
pub(super) fn trap_boundary(
    record: &SessionRecord,
    delivery: &dyn ChannelDelivery,
    boundary: &OpenBoundary,
    reason: &str,
) -> ResizeOutcome {
    let channel_id = record.channel_id();
    match delivery.stream_emission() {
        Some(emission) => emission.trap_core(channel_id),
        None => tracing::error!(%channel_id, "a trapped core has no emitter to latch fail-closed"),
    }
    tracing::warn!(
        session_id = %record.session_id(),
        %channel_id,
        resize_seq = boundary.seq,
        reason,
        "terminal.core_failed: a resize boundary could not be proven and the core is fail-closed"
    );
    ResizeOutcome::Trapped {
        reason: reason.to_owned(),
    }
}

pub(super) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// History a resize cost, reported once the record lock is gone so the warning
/// lands at the release the code actually performs.
pub(super) struct CaptureLoss {
    floor: u64,
    rows: u64,
}

pub(super) fn report_capture_loss(channel_id: u16, loss: Option<CaptureLoss>) {
    if let Some(loss) = loss {
        tracing::warn!(
            channel_id,
            history_floor = loss.floor,
            replay_lost_rows = loss.rows,
            "this resize moved the history floor: rows a never-resized session would still hold are gone"
        );
    }
}

/// The two counters a pin is computed from.
struct CoreCounters {
    discarded: u64,
    total: u64,
}

impl CoreCounters {
    fn read(record: &SessionRecord) -> Self {
        let discarded = record.terminal_core.discarded_line_count().unwrap_or(0);
        Self {
            discarded,
            total: discarded + record.terminal_core.scrollback_count() as u64,
        }
    }

    /// The rows the core still holds, as opposed to the `total` it has ever
    /// held: a pin's fresh count is a LOSS measure.
    fn retained(&self) -> u64 {
        self.total.saturating_sub(self.discarded)
    }
}
