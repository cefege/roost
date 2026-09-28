//! The history pin a core geometry change leaves behind: `pin_for` for a core
//! resized in place and `pin_for_adoption` for one rebuilt from a keeper
//! survivor. Split verbatim out of `session::resize` at the pin/boundary seam
//! (v2 counterpart: the boundary half of `apps/worker/src/session/
//! session-resize-capture.ts`); called by `session::resize::resize_in_place`
//! and `session::resume_core`.

use super::history::SbOriginPin;

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
