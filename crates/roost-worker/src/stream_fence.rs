//! Stream generation fencing, and the synchronized-output hold. Owned by the
//! worker.
//!
//! Both exist because a terminal can be replaced WHILE work for the old one is
//! still in flight, and the failure is a terminal that paints cells from a
//! generation that no longer exists.
//!
//! The fence is the general answer: every unit of scheduled work carries the
//! generation it was scheduled for, and work whose generation is stale is
//! dropped at the moment it would have been delivered. Not "ignored quietly" —
//! dropped, counted, and attributed, so a diagnostic snapshot says which
//! generation was superseded and how much work it still had outstanding.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a synchronized-output hold may withhold frames while SILENT.
///
/// A stream that opens a synchronized frame and never closes it — a TUI killed
/// mid-repaint, a truncated recording, a `printf` that emitted only the opener —
/// would otherwise withhold forever, and the browser goes dark while the core
/// keeps parsing. One second is long enough that a legitimate multi-chunk
/// repaint completes inside it and short enough that a user reads the result as
/// a hitch rather than a hang.
pub const SYNC_OUTPUT_MAX_SILENT: Duration = Duration::from_secs(1);

/// How many rows the browser may fall behind inside ONE synchronized frame,
/// counted as scrollback lines appended plus currently dirty viewport rows.
///
/// The second ceiling, and it exists because the two stuck shapes are
/// different: a hold that goes SILENT produces no further chunks, so nothing
/// re-evaluates it and only a timer can rescue it. A hold that keeps FLOODING is
/// caught here, well before the wall ceiling. Two thousand rows is about
/// eighty-three full 80x24 repaints — more than a second of 60fps full-screen
/// redraw — so a real synchronized frame never approaches it.
pub const SYNC_OUTPUT_MAX_PENDING_ROWS: u64 = 2_000;

/// A channel's current stream generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Generation(pub u64);

/// Work scheduled for a specific generation of a specific channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    pub channel_id: u16,
    pub generation: Generation,
    /// What to deliver if the work is still current when it comes due.
    pub payload: Vec<u8>,
    pub queued_at: Instant,
}

impl Scheduled {
    /// Whether this work is still wanted.
    pub fn is_current(&self, current: Generation) -> bool {
        self.generation == current
    }

    /// How long this work has been waiting.
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.queued_at)
    }
}

/// The current generation of every channel, and the work scheduled against it.
#[derive(Debug, Default)]
pub struct Fence {
    current: HashMap<u16, Generation>,
    /// The highest generation each channel has EVER used, kept across a close.
    ///
    /// Without this a reopened channel restarts at generation one and every
    /// unit of work still queued from before the close becomes current AGAIN —
    /// so the fence passes the very frames it exists to drop, and the terminal
    /// paints a generation that ended before it was reopened.
    highest: HashMap<u16, Generation>,
    /// Work that has been fenced out, by reason. Diagnostics only — a
    /// generation that keeps producing superseded work is a bug upstream, and
    /// the count is how an operator finds it.
    dropped_superseded: u64,
    dropped_unknown_channel: u64,
}

impl Fence {
    pub fn new() -> Self {
        Self::default()
    }

    /// The channel's current generation, or `None` if it has none.
    pub fn current(&self, channel_id: u16) -> Option<Generation> {
        self.current.get(&channel_id).copied()
    }

    /// Start a channel's stream, above any generation it has used before.
    pub fn open(&mut self, channel_id: u16) -> Generation {
        self.bump(channel_id)
    }

    /// Replace a channel's stream, returning the generation that superseded it.
    ///
    /// Called on every reason a stream is replaced: a resize that starts a new
    /// generation, a reattach, a repair. Work scheduled against the old
    /// generation is fenced out from this moment, not from when it would have
    /// run.
    pub fn replace(&mut self, channel_id: u16) -> Generation {
        self.bump(channel_id)
    }

    fn bump(&mut self, channel_id: u16) -> Generation {
        let next = self
            .highest
            .get(&channel_id)
            .copied()
            .unwrap_or(Generation(0))
            .0
            + 1;
        let generation = Generation(next);
        self.highest.insert(channel_id, generation);
        self.current.insert(channel_id, generation);
        generation
    }

    /// The channel is finished. Any later work for it is for a channel that no
    /// longer exists, which is the same class of error as a stale generation.
    pub fn close(&mut self, channel_id: u16) {
        self.current.remove(&channel_id);
    }

    /// Whether work for this channel and generation should be delivered.
    pub fn accepts(&self, work: &Scheduled) -> bool {
        match self.current.get(&work.channel_id) {
            Some(current) => work.is_current(*current),
            None => {
                // No current generation means the channel is closed, or was
                // never opened. Either way the work is not wanted.
                false
            }
        }
    }

    /// Take the work, dropping whatever the fence rejects.
    ///
    /// Returns only the work that is still wanted, and records what it dropped
    /// so the diagnostic snapshot can attribute a stall.
    pub fn take_current(&mut self, queued: Vec<Scheduled>) -> Vec<Scheduled> {
        let mut accepted = Vec::with_capacity(queued.len());
        for work in queued {
            match self.current.get(&work.channel_id) {
                None => self.dropped_unknown_channel += 1,
                Some(current) if work.generation != *current => self.dropped_superseded += 1,
                Some(_) => accepted.push(work),
            }
        }
        accepted
    }

    /// How much work has been dropped as superseded.
    pub fn dropped_superseded(&self) -> u64 {
        self.dropped_superseded
    }

    /// How much work has been dropped for a channel with no generation.
    pub fn dropped_unknown_channel(&self) -> u64 {
        self.dropped_unknown_channel
    }
}

/// What a synchronized-output hold is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldAction {
    /// Keep withholding. The hold is inside both ceilings.
    Withhold,
    /// The hold outlived its SILENT ceiling. The withheld frame ships and the
    /// stuck generation is bypassed.
    Release,
}

/// A synchronized-output hold on one channel's frames.
#[derive(Debug)]
pub struct SyncOutputHold {
    open_since: Option<Instant>,
    /// Rows the browser has fallen behind inside this hold: scrollback lines
    /// appended plus dirty viewport rows.
    pending_rows: u64,
}

impl SyncOutputHold {
    pub fn new() -> Self {
        Self {
            open_since: None,
            pending_rows: 0,
        }
    }

    /// Whether any hold is open.
    pub fn is_open(&self) -> bool {
        self.open_since.is_some()
    }

    /// A stream opened a synchronized frame: withhold from now.
    pub fn open(&mut self, now: Instant) {
        if self.open_since.is_none() {
            self.open_since = Some(now);
            self.pending_rows = 0;
        }
    }

    /// The stream closed the frame: resume.
    pub fn close(&mut self) {
        self.open_since = None;
        self.pending_rows = 0;
    }

    /// Note output the browser has not yet been given.
    pub fn note_pending(&mut self, rows: u64) {
        if self.is_open() {
            self.pending_rows = self.pending_rows.saturating_add(rows);
        }
    }

    /// What to do about the hold right now.
    ///
    /// Two INDEPENDENT ceilings, because a stuck hold has two different shapes
    /// and neither ceiling catches the other: one that goes silent produces no
    /// further chunks, so nothing re-evaluates it and only the timer can rescue
    /// it; one that keeps flooding is caught by the row count long before the
    /// timer fires.
    pub fn action(&self, now: Instant) -> HoldAction {
        let Some(since) = self.open_since else {
            return HoldAction::Release;
        };
        if self.pending_rows >= SYNC_OUTPUT_MAX_PENDING_ROWS {
            return HoldAction::Release;
        }
        if now.saturating_duration_since(since) >= SYNC_OUTPUT_MAX_SILENT {
            return HoldAction::Release;
        }
        HoldAction::Withhold
    }
}

impl Default for SyncOutputHold {
    fn default() -> Self {
        Self::new()
    }
}
