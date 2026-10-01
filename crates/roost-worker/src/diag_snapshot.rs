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
//! Ports v2 `apps/worker/src/browser-commands/browser-command-diag.ts`, `apps/worker/src/session/session-diag-snapshot.ts`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use roost_observability::{EventClock, SystemClock};

use crate::session::lifecycle::SessionTable;
use crate::session::types::SessionRecord;
use crate::session::unhandled_seq::{UnhandledSequenceSnapshot, unhandled_sequence_snapshot};

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
    /// Escape sequences this channel's core dropped — the "renders wrong in
    /// Roost, fine elsewhere" lane. Sampled here as well as on the emit path so
    /// a parked pane, which emits no frames, still answers. `None` = nothing
    /// logged, which is not proof of full support.
    pub unhandled_sequences: Option<UnhandledSequenceSnapshot>,
}

/// The report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The wall clock, for placing the report in time. NOT used for any age.
    pub captured_at: Duration,
    /// The one monotonic reading every age in this report is measured against.
    pub mono_now: Instant,
    pub channels: HashMap<u16, ChannelDiag>,
    /// One entry per live session, keyed by session id.
    ///
    /// RENDERED HERE AND NOT DOWNSTREAM because the per-session entry needs the
    /// record and the emitter read together, under this report's one monotonic
    /// reading; a renderer that took a second reading would stamp the two
    /// halves of the same fact from different instants.
    pub sessions: BTreeMap<String, serde_json::Value>,
}

impl Snapshot {
    /// Start a report. The monotonic reading is taken ONCE, here, and every age
    /// below is measured against it.
    pub fn begin(captured_at: Duration, mono_now: Instant) -> Self {
        Self {
            captured_at,
            mono_now,
            channels: HashMap::new(),
            sessions: BTreeMap::new(),
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
            sessions: BTreeMap::new(),
        }
    }
    /// The report as the WORKER's own live sessions make it.
    ///
    /// The fold lives here rather than in the caller that owns a session table
    /// because the monotonic reading is this module's rule and not the
    /// caller's: it is taken ONCE, in this function, and every age in the
    /// report is measured against it. A caller that stamped each channel from
    /// its own reading would satisfy the signature and break the property.
    ///
    /// The per-session entries are folded in the SAME pass and off the SAME
    /// record borrow as the channel report, because a session that closed
    /// between two loops would then appear in one half of the report and not
    /// the other.
    pub fn of_live_sessions(
        table: &SessionTable,
        worker_fp: &str,
        cells: &Mutex<dyn crate::session::binding::CellDelivery>,
    ) -> Self {
        let captured_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        let mono_now = Instant::now();
        // The unhandled-sequence sampler stamps a first sighting with the
        // process monotonic clock, the same clock the emit path samples with.
        let mono_ms = SystemClock.mono_ns() / 1_000_000;
        let mut channels: HashMap<u16, ChannelDiag> = HashMap::new();
        let mut sessions: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        for (session_id, _) in table.live() {
            // `with_record_mut` nests: `None` is "no such session", so the
            // capability's own `Option` is the inner layer, and a session that
            // closed between the two reads is simply absent from the report.
            // Mutable because sampling advances the unhandled log's mark.
            let Some(entry) = table.with_record_mut(&session_id, |record| {
                let channel = channel_diag(record, mono_ms)?;
                let emitter = match cells.lock() {
                    Ok(guard) => guard.channel_diagnostics(record.channel_id()),
                    Err(poisoned) => poisoned
                        .into_inner()
                        .channel_diagnostics(record.channel_id()),
                };
                let value = crate::session::diagnostics::session_value(
                    record,
                    worker_fp,
                    &emitter,
                    channel
                        .unhandled_sequences
                        .as_ref()
                        .map(crate::browser_commands::diagnostics::unhandled_json),
                    mono_ms,
                );
                Some((channel, value))
            }) else {
                continue;
            };
            if let Some((channel, value)) = entry {
                channels.insert(channel.channel_id, channel);
                sessions.insert(session_id.as_str().to_owned(), value);
            }
        }
        Self {
            captured_at,
            mono_now,
            channels,
            sessions,
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

/// One channel's diagnostic state, read once and never revisited.
fn channel_diag(record: &mut SessionRecord, mono_ms: u64) -> Option<ChannelDiag> {
    let ring = record.scrollback.len() as u64;
    let cap = record.scrollback.capacity() as u64;
    Some(ChannelDiag {
        // The report is keyed by the u16 the wire names, and the brand is a
        // `u32` newtype: a channel id past `u16::MAX` is a keeper that has
        // opened more channels than the wire can name, so it saturates rather
        // than wrapping onto a real channel's number.
        channel_id: u16::try_from(record.channel_id().as_u32()).unwrap_or(u16::MAX),
        grid_epoch: record.cell_emit.grid_epoch(),
        generation: record.cell_emit.seq,
        suppression: None,
        ring: Some(RingBounds {
            retained_bytes: ring,
            cap_bytes: cap,
            evicting: record.scrollback.evicting(),
        }),
        unhandled_sequences: unhandled_sequence_snapshot(record, mono_ms),
    })
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
                    unhandled_sequences: None,
                })
                .suppression = Some(suppression);
        }
        Snapshot {
            captured_at,
            mono_now,
            channels,
            sessions: BTreeMap::new(),
        }
    }
}
