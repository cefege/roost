//! Geometry: the in-place resize at the keeper's ordered `ResizeAck` boundary,
//! the history pin that resize writes, and the recovery for a lost
//! acknowledgement. `session::control_lanes` serialises the write, the delivery
//! seam freezes the core, and the coordinator's own SCD decides the shape.
//! Depends on `roost_term` for the core, `roost_keeper` for the geometry
//! vocabulary and `super::lifecycle` for the table — and on nothing that
//! depends on it back.
//!
//! THE RESIZE IS IN PLACE, AT THE BOUNDARY. Earlier bytes parse at the old size,
//! `TerminalCore::resize` runs before the callback returns, and later bytes parse
//! at the new size. That is the whole of `docs/FAILURE-INDEX.md` "Scrollback
//! mangles or drifts with no user action": the alternative — rebuilding the
//! emulator from a bounded byte ring after every resize — cannot recover bytes
//! already evicted and silently converts unchanged cells into blanks. A resize
//! forces a complete NEW-STREAM baseline and never reconstructs ordinary live
//! state; worker-history replay stays reserved for genuine process adoption.
//!
//! BYTES PRODUCED WHILE THE BOUNDARY IS UNRESOLVED ARE CAPTURED, NOT PARSED. A
//! PTY does not stop talking because a SIGWINCH is in flight, and parsing them
//! at a geometry the keeper has not acknowledged is how a half-applied resize
//! paints a grid that was never on that terminal. The capture retains them, so
//! history loses nothing, and replays them at the boundary.

use roost_protocol::wire::brand::ChannelId;
use roost_term::TerminalCore;

use super::history::SbOriginPin;
use super::lifecycle::SessionManager;
use super::types::SessionRecord;
use crate::browser_commands::Refusal;

/// The history pin's inputs, read at the moment a core's geometry changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinInputs {
    pub at_mono_ms: u64,
    pub cols: u16,
    pub rows: u16,
    /// Whether a replacement core was replayed from the retained ring, rather
    /// than the existing core being resized where it stands.
    pub replayed_ring: bool,
    /// Whether the replay source had itself already evicted.
    pub ring_evicted: bool,
    /// Lines the OLD core had already dropped, read before the change.
    pub prev_dropped: u64,
    /// Lines the OLD core held, read before the change.
    pub prev_total: u64,
    /// Lines the core dropped after the change.
    pub fresh_discarded: u64,
    /// Lines the core holds after the change.
    pub fresh_count: u64,
    /// The highest floor a replay bound has already established here.
    pub previous_replay_floor: u64,
}

/// What a resize did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResizeOutcome {
    /// The core was resized in place and the capture replayed behind it.
    Applied { cols: u16, rows: u16 },
    /// The channel was already at this geometry: nothing was written to the
    /// keeper, no sequence was spent and no history floor moved.
    Unchanged,
    /// The keeper refused the geometry, or the boundary could not be proven.
    ///
    /// The captured bytes are parsed at the geometry still in force and the
    /// session keeps running: a resize that cannot be proven is a resize that
    /// did not happen.
    Refused { reason: String },
}

/// The pin a core change establishes.
///
/// A rebuild is the one moment Roost's line numbering is RE-DERIVED rather than
/// advanced: a replacement core restarts its discarded counter at zero while a
/// core resized in place retains it. `sb_origin` absorbs either difference so a
/// browser's absolute row indexes never re-alias, and `sb_dropped` is the floor
/// that results — which is what a diagnostic reads back as
/// `live_discarded - sb_origin`, and what a page read compares its window
/// against.
pub fn pin_for(inputs: PinInputs) -> SbOriginPin {
    let deficit =
        inputs.prev_total as i128 - inputs.fresh_discarded as i128 - inputs.fresh_count as i128;
    let clamped = deficit < 0;
    let sb_dropped = deficit.max(0) as u64;
    let sb_origin = inputs.fresh_discarded.saturating_sub(sb_dropped);
    // Rows the old numbering had and the fresh one does not: history lost to a
    // REPLAY BOUND rather than to eviction, because a session that was never
    // resized would still hold them.
    let replay_lost_rows = sb_dropped.saturating_sub(inputs.prev_dropped);
    let replay_floor = if replay_lost_rows > 0 {
        inputs.previous_replay_floor.max(sb_dropped)
    } else {
        inputs.previous_replay_floor
    };
    SbOriginPin {
        at_mono_ms: inputs.at_mono_ms,
        cols: inputs.cols,
        rows: inputs.rows,
        replayed_ring: inputs.replayed_ring,
        ring_evicted: inputs.ring_evicted,
        prev_dropped: inputs.prev_dropped,
        prev_total: inputs.prev_total,
        fresh_discarded: inputs.fresh_discarded,
        fresh_count: inputs.fresh_count,
        sb_origin,
        sb_dropped,
        clamped,
        replay_lost_rows,
        replay_floor,
    }
}

/// The pin a rebuilt-from-survivor core starts from.
///
/// There is no previous core on this worker to be continuous with, so nothing
/// is lost to a replay bound: the whole of the keeper's window was replayed, the
/// floor starts at zero, and the origin IS the core's own discarded count so
/// the first frame a client reads already has absolute row indexes that mean
/// something. What the session lost while the worker was gone is the KEEPER's
/// eviction, which `ring_evicted` records.
pub fn pin_for_adoption(
    at_mono_ms: u64,
    cols: u16,
    rows: u16,
    ring_evicted: bool,
    discarded: u64,
    retained: u64,
) -> SbOriginPin {
    SbOriginPin {
        at_mono_ms,
        cols,
        rows,
        replayed_ring: true,
        ring_evicted,
        prev_dropped: 0,
        prev_total: 0,
        fresh_discarded: discarded,
        fresh_count: retained,
        sb_origin: discarded,
        sb_dropped: 0,
        clamped: false,
        replay_lost_rows: 0,
        replay_floor: 0,
    }
}

impl SessionManager {
    /// Move a live channel's geometry, in place, at the keeper's boundary.
    ///
    /// The capture opens BEFORE the write and closes AFTER the acknowledgement,
    /// and the whole apply runs under the record's lock, so a chunk cannot be
    /// parsed at the new geometry ahead of the bytes that preceded it.
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
        let mut record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session_id = record.session_id().clone();
        let already = (record.terminal_core.cols(), record.terminal_core.rows());
        if already == (cols, rows) {
            return Ok(ResizeOutcome::Unchanged);
        }
        let seq = self.take_resize_seq(raw);
        let delivery = self
            .ingest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !delivery.freeze_capture(channel_id) {
            return Err(Refusal::failed(
                "resize",
                format!("channel {raw} already has an unresolved resize in flight"),
            ));
        }
        if let Err(fault) = self.keeper.resize_channel(raw, seq, cols, rows) {
            let outcome = self.close_capture(
                &mut record,
                &*delivery,
                None,
                "the keeper refused the resize",
            );
            return Ok(outcome);
        }
        let outcome = self.close_capture(
            &mut record,
            &*delivery,
            Some((cols, rows)),
            "the resize was applied at the keeper's boundary",
        );
        drop(delivery);
        drop(record);
        tracing::info!(
            session_id = %session_id,
            channel_id = raw,
            from = ?already,
            to = ?((cols, rows)),
            outcome = ?outcome,
            "a session's geometry was taken to the keeper and settled at its boundary"
        );
        Ok(outcome)
    }

    /// Close a capture and put the bytes it held where they belong.
    ///
    /// `target` is the geometry the boundary proved, or `None` when it proved
    /// nothing: the captured bytes are then parsed at the geometry still in
    /// force, which is the only honest reading of an unproven boundary.
    fn close_capture(
        &self,
        record: &mut SessionRecord,
        delivery: &dyn super::binding::ChannelDelivery,
        target: Option<(u16, u16)>,
        reason: &'static str,
    ) -> ResizeOutcome {
        let channel_id = record.channel_id();
        let session_id = record.session_id().clone();
        let captured = delivery.close_capture(channel_id);
        let at = self.clock.mono_ns() / 1_000_000;
        if captured.overflowed {
            // More output arrived while the boundary was unresolved than the
            // retained window holds. History keeps every byte, but the core
            // cannot be brought forward across a gap that wide, so the geometry
            // is not changed and the stream is reported as needing a rebuild.
            tracing::error!(
                session_id = %session_id,
                channel_id = channel_id.as_u32(),
                captured_bytes = captured.bytes,
                "a resize boundary captured more output than the retained window holds; \
                 the geometry was left alone"
            );
            return ResizeOutcome::Refused {
                reason: format!("{reason}, and the capture outgrew the retained window"),
            };
        }
        let (cols, rows) =
            target.unwrap_or_else(|| (record.terminal_core.cols(), record.terminal_core.rows()));
        let before = CoreCounters::read(record);
        record.terminal_core.resize(cols, rows);
        record.terminal_core.write(&captured.bytes);
        let after = CoreCounters::read(record);
        let pin = pin_for(PinInputs {
            at_mono_ms: at,
            cols: record.terminal_core.cols(),
            rows: record.terminal_core.rows(),
            replayed_ring: false,
            ring_evicted: record.scrollback.evicting(),
            prev_dropped: before.discarded,
            prev_total: before.total,
            fresh_discarded: after.discarded,
            fresh_count: after.count,
            previous_replay_floor: record
                .sb_origin_pin
                .map_or(0, |previous| previous.replay_floor),
        });
        let lost = pin.replay_lost_rows;
        let floor = pin.sb_dropped;
        record.sb_origin_pin = Some(pin);
        record.cell_emit.grid_epoch_revision += 1;
        let applied = (record.terminal_core.cols(), record.terminal_core.rows());
        drop(record);
        if lost > 0 {
            tracing::warn!(
                session_id = %session_id,
                channel_id = channel_id.as_u32(),
                history_floor = floor,
                replay_lost_rows = lost,
                "this resize moved the history floor: rows a never-resized session would \
                 still hold are gone"
            );
        }
        match target {
            Some(_) => ResizeOutcome::Applied {
                cols: applied.0,
                rows: applied.1,
            },
            None => ResizeOutcome::Refused {
                reason: reason.to_string(),
            },
        }
    }

    /// The next sequence this worker asks the keeper to apply on a channel.
    ///
    /// Per channel and monotonic, because the keeper rejects a sequence it has
    /// already applied: a reused one would be a resize the keeper believes it
    /// has done.
    fn take_resize_seq(&self, channel_id: u16) -> u64 {
        let mut seqs = self
            .resize_seqs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = seqs.entry(channel_id).or_insert(0);
        *next += 1;
        *next
    }

    /// Record the sequence the keeper has actually applied, which is what an
    /// adoption starts from: this worker's own counter begins at zero and the
    /// keeper rejects a sequence below the one it has already applied.
    pub fn note_applied_resize_seq(&self, channel_id: u16, applied_seq: u64) {
        self.resize_seqs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(channel_id, applied_seq);
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
}
