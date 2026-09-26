//! Truthful, on-demand state for the coordinator's diag fan-out. Owned by the
//! worker.
//!
//! Two properties make this worth a module, and both are about the report
//! being TRUSTWORTHY rather than merely present.
//!
//! EVERY AGE IS MEASURED AGAINST ONE MONOTONIC READING. The wall clock stamps
//! the report so a human can place it in time; every age inside it comes from a
//! monotonic clock taken once. A host clock step — an NTP correction, a
//! suspend and resume — then moves the stamp without moving the ages, so it
//! cannot make a stall appear or make one vanish. Taking a fresh reading per
//! field is the obvious implementation and it is wrong for exactly that reason.
//!
//! A STALL IS ATTRIBUTABLE FROM THE SNAPSHOT ALONE. A gate that has been
//! withholding frames records which gate, since when, and how many frames it
//! suppressed. An operator reads one report instead of correlating logs, and a
//! gate past its own ceiling is marked rather than left to be inferred from a
//! timeout somewhere else.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Which gate is withholding a channel's cell frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gate {
    /// A resize transaction is capturing the grid before it resizes.
    ResizeCapture,
    /// The first full after a generation change.
    Baseline,
    /// A synchronized-output frame is open.
    SyncOutput,
}

impl Gate {
    /// What the gate is measured against.
    ///
    /// Per gate, and not one shared number: a synchronized-output hold answers
    /// to its own cap, and composing it with a resize gate's budget is how a
    /// hang neither admits to gets built. A resize gate that installs itself
    /// retires any open hold on the way in, precisely so the two ceilings never
    /// compose.
    pub fn budget(self) -> Duration {
        match self {
            Gate::ResizeCapture | Gate::Baseline => Duration::from_millis(250),
            Gate::SyncOutput => Duration::from_secs(1),
        }
    }
}

/// A gate withholding a channel's frames, and what it has cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateSuppression {
    pub gate: Gate,
    /// When the hold opened, on the report's monotonic clock.
    pub since: Instant,
    /// How many frames this gate has suppressed.
    pub frames: u32,
}

/// Whether a gate is still within its own ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    Within,
    /// The gate outlived its ceiling.
    ///
    /// Over budget on the RESIZE path means the transaction is corrupt; on the
    /// synchronized path it means the withheld frame shipped and the stuck
    /// generation was bypassed. Same state, different meanings — which is why
    /// the gate is recorded alongside it rather than collapsed to a boolean.
    Over,
}

/// A ring's occupancy, and whether it is at its cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingBounds {
    pub retained_bytes: u64,
    pub cap_bytes: u64,
    /// The ring is at its cap and evicting. This is the flag that makes a
    /// scrollback stall attributable: without it, "the terminal stopped
    /// scrolling" and "the ring is full" are indistinguishable.
    pub evicting: bool,
}

/// One channel's diagnostic state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDiag {
    pub channel_id: u16,
    pub grid_epoch: String,
    /// The stream generation this state describes. A report carrying an
    /// outdated generation is worse than no report.
    pub generation: u64,
    /// A gate currently withholding frames, if any.
    pub suppression: Option<GateSuppression>,
    pub ring: Option<RingBounds>,
}

/// The report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The wall clock, for placing the report in time. NOT used for any age.
    pub captured_at: Duration,
    /// The one monotonic reading every age in this report is measured against.
    pub mono_now: Instant,
    pub channels: HashMap<u16, ChannelDiag>,
}

impl Snapshot {
    /// Start a report. The monotonic reading is taken ONCE, here, and every age
    /// below is measured against it.
    pub fn begin(captured_at: Duration, mono_now: Instant) -> Self {
        Self {
            captured_at,
            mono_now,
            channels: HashMap::new(),
        }
    }

    /// Fold a set of recorded channels into the report.
    pub fn with_channels(
        captured_at: Duration,
        mono_now: Instant,
        channels: HashMap<u16, ChannelDiag>,
    ) -> Self {
        Self {
            captured_at,
            mono_now,
            channels,
        }
    }
    /// One channel's recorded state, if the report carries it.
    pub fn channel(&self, channel_id: u16) -> Option<&ChannelDiag> {
        self.channels.get(&channel_id)
    }

    /// How long a gate has been withholding, against the report's reading.
    pub fn suppression_age(&self, channel_id: u16) -> Option<Duration> {
        let suppression = self.channel(channel_id)?.suppression?;
        Some(self.mono_now.saturating_duration_since(suppression.since))
    }

    /// Whether a gate has outlived its own ceiling.
    pub fn suppression_budget(&self, channel_id: u16) -> Option<Budget> {
        let suppression = self.channel(channel_id)?.suppression?;
        let age = self.mono_now.saturating_duration_since(suppression.since);
        Some(if age >= suppression.gate.budget() {
            Budget::Over
        } else {
            Budget::Within
        })
    }

    /// Channels whose gate has outlived its ceiling.
    pub fn over_budget(&self) -> Vec<(u16, Gate, Duration)> {
        self.channels
            .iter()
            .filter_map(|(channel_id, channel)| {
                let suppression = channel.suppression?;
                let age = self.mono_now.saturating_duration_since(suppression.since);
                (age >= suppression.gate.budget()).then_some((*channel_id, suppression.gate, age))
            })
            .collect()
    }

    /// Channels evicting, which is where a scrollback stall usually is.
    pub fn evicting(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.ring.is_some_and(|ring| ring.evicting))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }
}

/// A gate that is currently withholding a channel's frames.
///
/// One holder per channel, and installing a gate RETIRES any open hold — a
/// resize transaction must not stack on top of a synchronized-output hold, or
/// the two ceilings compose into a hang that neither one admits to.
#[derive(Debug, Default)]
pub struct GateTracker {
    open: HashMap<u16, GateSuppression>,
}

impl GateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a gate on a channel, replacing whatever was there.
    pub fn open(&mut self, channel_id: u16, gate: Gate, now: Instant) {
        self.open.insert(
            channel_id,
            GateSuppression {
                gate,
                since: now,
                frames: 0,
            },
        );
    }

    /// Count one suppressed frame against the open gate.
    pub fn suppress(&mut self, channel_id: u16) -> u32 {
        let Some(suppression) = self.open.get_mut(&channel_id) else {
            return 0;
        };
        suppression.frames += 1;
        suppression.frames
    }

    /// The gate closed; frames flow again.
    pub fn release(&mut self, channel_id: u16) {
        self.open.remove(&channel_id);
    }

    pub fn suppression(&self, channel_id: u16) -> Option<GateSuppression> {
        self.open.get(&channel_id).copied()
    }

    pub fn open_channels(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self.open.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Fold the tracker into a report, against the report's reading.
    pub fn into_snapshot(
        self,
        captured_at: Duration,
        mono_now: Instant,
        mut channels: HashMap<u16, ChannelDiag>,
    ) -> Snapshot {
        for (channel_id, suppression) in self.open {
            channels
                .entry(channel_id)
                .or_insert(ChannelDiag {
                    channel_id,
                    grid_epoch: String::new(),
                    generation: 0,
                    suppression: None,
                    ring: None,
                })
                .suppression = Some(suppression);
        }
        Snapshot {
            captured_at,
            mono_now,
            channels,
        }
    }
}
