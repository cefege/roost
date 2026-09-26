//! What a session knows about its own history: the floor the last core rebuild
//! established, and the escape sequences this core instance did not recognise.
//! `session::resize` writes the pin, `session::emit` samples the log, and the
//! diagnostic snapshot reads both. Depends on nothing but the standard library —
//! and on nothing that depends on it back.
//!
//! These two live together because they are the same question asked twice: what
//! does this session's grid NOT contain that a client might believe it does? One
//! answer is history a rebuild could not recover, the other is bytes the core
//! never parsed. Everything here is DIAGNOSTIC; the emit path reads neither, and
//! a diagnostic that could change what a client paints is a diagnostic that can
//! itself be the outage.

/// How many distinct unhandled escape sequences one core reports.
///
/// The ring is bounded because a terminal that emits a novel sequence per
/// frame would otherwise grow this record without limit, and a diagnostic
/// surface that can itself be the outage is no diagnostic at all.
pub const UNHANDLED_SEQ_MAX: usize = 32;

/// One distinct escape sequence the core's dispatcher did not recognise, as
/// first seen on the CURRENT core instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnhandledSequenceEntry {
    /// The CSI final byte, e.g. `q` for DECSCUSR.
    pub final_byte: String,
    /// The private-parameter prefix (`?`, `>`, `<`, `=`), or empty when absent.
    pub private: String,
    /// How many parameters the sequence carried.
    ///
    /// A count rather than the parameters themselves: the core records at most
    /// four, so a longer sequence would otherwise look shorter than it is.
    pub param_count: u32,
    /// The first four parameters, as the core parsed them.
    pub params: Vec<u32>,
    /// Monotonic milliseconds at which this core first produced it.
    pub first_seen_mono_ms: u64,
}

/// One core instance's unhandled-sequence ring, as Roost accumulates it.
///
/// The core's own ring is never cleared, so `consumed` — its logged total as of
/// the last sample — is the high-water mark that stops a stale entry being
/// reported twice, and `keys` dedupes repeats of the same sequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnhandledSequenceLog {
    /// Sequences the core had logged in total when last sampled, duplicates
    /// included. Nothing at or below this index is ever reported again.
    pub consumed: u64,
    /// Distinct sequences observed, oldest first, capped at
    /// [`UNHANDLED_SEQ_MAX`].
    pub entries: Vec<UnhandledSequenceEntry>,
    /// The dedupe key of every recorded entry.
    pub keys: Vec<String>,
    /// Entries the core's ring overwrote between two samples while this
    /// accumulator was recording, so Roost never saw them: only their
    /// existence is knowable, not what they were.
    pub ring_dropped: u32,
    /// `entries` reached the cap; later distinct sequences are not recorded.
    pub capped: bool,
}

/// The last core-rebuild origin pin, and the history floor it established.
///
/// A rebuild is the one moment Roost's monotonic numbering is RE-DERIVED rather
/// than advanced: a replacement core restarts its discarded counter at zero
/// while a preserved alternate core retains it. This absorbs either difference
/// so browser-held absolute row indexes never re-alias. It is also the only
/// moment history can vanish for a reason other than eviction — a bounded
/// replay can rebuild shallower history and move the floor.
///
/// One fixed record per session, overwritten in place by its single writer, so
/// sampling it is O(1) and it retains no history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbOriginPin {
    /// Monotonic ms at the pin. Age, never a wall clock, so a host clock step
    /// cannot forge or hide how long ago the floor moved.
    pub at_mono_ms: u64,
    pub cols: u16,
    pub rows: u16,
    /// Whether the full retained byte ring was replayed into a replacement core,
    /// rather than a frozen alternate core caught up in place.
    pub replayed_ring: bool,
    /// Whether the capture's recorded boundary had already fallen out of the
    /// ring.
    pub ring_evicted: bool,
    /// The OLD core read live at the pin: the floor and monotonic total the pin
    /// has to reproduce.
    pub prev_dropped: u64,
    pub prev_total: u64,
    /// Core counters after the replay or the in-place alternate resize.
    pub fresh_discarded: u64,
    pub fresh_count: u64,
    /// The pin's outputs: the additive origin and the floor it establishes.
    pub sb_origin: u64,
    pub sb_dropped: u64,
    /// `prev_total - fresh_discarded - fresh_count` went NEGATIVE and was
    /// clamped: the replay rebuilt MORE lines than the old core reported, so
    /// numbering continuity was discarded instead of preserved.
    pub clamped: bool,
    /// Rows that existed under the old core's numbering and do not exist under
    /// the fresh one — history lost to the REPLAY BOUND, not to eviction.
    pub replay_lost_rows: u64,
    /// The highest floor a replay bound has ever established here, carried
    /// across pins. The floor only rises, so one comparison classifies the
    /// CURRENT floor.
    pub replay_floor: u64,
}
