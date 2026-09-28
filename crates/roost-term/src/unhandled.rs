//! The fixed ring of escape sequences a core's dispatcher did not recognise.
//! A [`crate::TerminalCore`] fills it while it parses; the worker's
//! `session::unhandled_seq` samples it against its own high-water mark and the
//! diagnostics snapshot reads the result. Ports the ring v2's `@wterm/core`
//! kept in `terminal.zig` (`DebugLogEntry`, `DEBUG_LOG_MAX`) and
//! `packages/wterm/src/wterm-core-factory.ts` decoded (`unhandledSequenceRing`).
//!
//! THE RING IS NEVER CLEARED, exactly like the one it replaces: `total` only
//! grows, so a reader holds its own mark against it and never re-reports an
//! entry. The window is the newest [`UNHANDLED_RING_CAPACITY`] entries; anything
//! older was overwritten, and `total` is what makes that loss countable.

/// Entries the ring retains. v2's core held 32, and the worker's accumulator is
/// sized to the same number so neither can remember what the other forgot.
pub const UNHANDLED_RING_CAPACITY: usize = 32;

/// Parameters recorded per entry. A longer sequence keeps its full
/// `param_count`, so two sequences agreeing on these four stay distinguishable.
pub const UNHANDLED_PARAMS_RECORDED: usize = 4;

/// One CSI sequence the dispatcher dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnhandledSequence {
    /// The CSI final byte, e.g. `b'q'`.
    pub final_byte: u8,
    /// The private marker (`<` `=` `>` `?`) in the first parameter position, or
    /// 0 when the sequence has none.
    pub private: u8,
    /// How many parameters the sequence carried, recorded or not.
    pub param_count: u16,
    /// The first value of each of the first [`UNHANDLED_PARAMS_RECORDED`]
    /// parameters; slots past `param_count` are zero and not part of the entry.
    pub params: [u16; UNHANDLED_PARAMS_RECORDED],
}

impl UnhandledSequence {
    /// The recorded parameters, without the unused slots.
    pub fn recorded_params(&self) -> &[u16] {
        let recorded = usize::from(self.param_count).min(UNHANDLED_PARAMS_RECORDED);
        &self.params[..recorded]
    }
}

/// A fixed-size, never-cleared ring of [`UnhandledSequence`]s.
///
/// Fixed so that recording one allocates nothing: this is filled from inside
/// the parser's dispatch, on the byte path of every session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnhandledSequenceRing {
    entries: [UnhandledSequence; UNHANDLED_RING_CAPACITY],
    total: u64,
}

impl Default for UnhandledSequenceRing {
    fn default() -> Self {
        Self {
            entries: [UnhandledSequence::default(); UNHANDLED_RING_CAPACITY],
            total: 0,
        }
    }
}

impl UnhandledSequenceRing {
    /// Sequences logged over the core's whole life, duplicates included.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// How many entries the window can hold.
    pub const fn capacity(&self) -> usize {
        UNHANDLED_RING_CAPACITY
    }

    /// The retained window, oldest first: the newest `min(total, capacity)`
    /// entries. Element `i` is logical entry `total - retained + i`.
    pub fn window(&self) -> impl Iterator<Item = &UnhandledSequence> + '_ {
        let capacity = UNHANDLED_RING_CAPACITY as u64;
        let oldest = self.total.saturating_sub(capacity);
        (oldest..self.total).map(move |logical| &self.entries[(logical % capacity) as usize])
    }

    /// Log one dropped sequence, overwriting the oldest once full.
    pub fn record(&mut self, sequence: UnhandledSequence) {
        let slot = (self.total % UNHANDLED_RING_CAPACITY as u64) as usize;
        self.entries[slot] = sequence;
        self.total = self.total.saturating_add(1);
    }
}
