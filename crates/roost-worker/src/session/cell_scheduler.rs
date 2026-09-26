//! Cell-emission cadence and gate suppression. One identity-fenced record per
//! channel, so a replaced stream generation cannot consume, cancel or re-arm
//! another generation's work. `runtime` owns the timer that calls
//! [`CellEmitter::emit_cell_frame`]; this file owns WHEN a channel is due, what
//! is withholding it, and the attribution.
//!
//! A gate is a named, bounded reason cell frames are NOT being produced. It
//! exists so a stalled emitter is attributable from the diagnostic snapshot
//! alone, instead of by correlating log lines and guessing which of three
//! conditions held. Every hold is announced and every release is announced:
//! a gate that opens and never closes is the browser going dark while the core
//! keeps parsing, and the wall ceiling that recovers it belongs to whoever owns
//! the synchronized-output hold.

use roost_protocol::wire::brand::ChannelId;

use super::emit::CellEmitter;

/// The ceiling on queued input-echo promotions per channel. A channel with no
/// active sink never consumes one, and past this many queued keystrokes another
/// promotion buys nothing — the next leading emit already carries them all.
pub const MAX_PENDING_INPUT_ECHO_PROMOTIONS: u8 = 8;

/// Which gate is withholding cell frames for a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellGate {
    /// A sequenced live resize has not resolved its boundary. Bytes are being
    /// retained for the rebuild and are NOT parsed at stale geometry.
    ResizeCapture,
    /// The application has an open synchronized-output block: it is telling the
    /// renderer not to paint a half-drawn grid.
    SyncOutput,
}

impl CellGate {
    pub fn as_str(self) -> &'static str {
        match self {
            CellGate::ResizeCapture => "resize_capture",
            CellGate::SyncOutput => "sync_output",
        }
    }
}

/// How long one gate has held a channel, and how many frames it swallowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateSuppression {
    pub gate: CellGate,
    /// When the CURRENT hold opened, in epoch milliseconds.
    pub since_ms: i64,
    /// How many emit attempts this hold has swallowed. A rising count on a
    /// channel that is otherwise healthy is the signature of a stuck hold.
    pub suppressed: u64,
}

impl CellEmitter {
    /// Withhold cell frames for a channel, attributing the hold.
    ///
    /// Re-holding with a DIFFERENT gate restarts the attribution rather than
    /// keeping the older one: the question a diagnostic answers is "what is
    /// holding this channel NOW", and a resize capture that hands over to a
    /// synchronized-output hold is a new wait.
    pub fn hold_frames(&mut self, channel_id: ChannelId, gate: CellGate, now_ms: i64) {
        let held = self.gates.entry(channel_id).or_insert(GateSuppression {
            gate,
            since_ms: now_ms,
            suppressed: 0,
        });
        if held.gate != gate {
            *held = GateSuppression {
                gate,
                since_ms: now_ms,
                suppressed: 0,
            };
            tracing::info!(%channel_id, gate = gate.as_str(), "a cell-emission gate was set");
        }
    }

    /// Release a hold. The channel owes an emit if anything is dirty, which the
    /// caller learns from [`CellEmitter::is_dirty`] rather than from a value
    /// returned here — a release is not a schedule.
    pub fn release_frames(&mut self, channel_id: ChannelId) {
        if let Some(held) = self.gates.remove(&channel_id) {
            tracing::info!(
                %channel_id,
                gate = held.gate.as_str(),
                suppressed = held.suppressed,
                "a cell-emission gate was released"
            );
        }
    }

    /// What is withholding a channel, and for how long it has been.
    pub fn gate_suppression(&self, channel_id: ChannelId) -> Option<GateSuppression> {
        self.gates.get(&channel_id).copied()
    }

    /// Whether a gate currently holds this channel.
    pub(crate) fn gate_held(&self, channel_id: ChannelId) -> bool {
        self.gates.contains_key(&channel_id)
    }

    /// Count one swallowed emit against the open hold.
    pub(crate) fn note_gate_suppression(&mut self, channel_id: ChannelId, _now_ms: i64) {
        if let Some(held) = self.gates.get_mut(&channel_id) {
            held.suppressed = held.suppressed.saturating_add(1);
        }
    }

    /// Dirty work observed. The cadence, not this type, decides when to run.
    pub fn note_dirty(&mut self, channel_id: ChannelId) {
        self.dirty.insert(channel_id);
    }

    pub fn is_dirty(&self, channel_id: ChannelId) -> bool {
        self.dirty.contains(&channel_id)
    }

    pub fn clear_dirty(&mut self, channel_id: ChannelId) {
        self.dirty.remove(&channel_id);
    }

    /// Every channel with work outstanding, for a cadence sweep.
    pub fn dirty_channels(&self) -> Vec<ChannelId> {
        let mut channels: Vec<ChannelId> = self.dirty.iter().copied().collect();
        channels.sort_unstable();
        channels
    }

    /// Queue a keystroke whose echo should not wait out the coalesce window.
    ///
    /// Bounded, because a channel with no active sink never consumes one, and
    /// an unbounded queue is a per-keystroke allocation on a path that runs
    /// while a user types.
    pub fn note_input_echo(&mut self, channel_id: ChannelId) {
        let queued = self.input_echo.entry(channel_id).or_insert(0);
        *queued = queued
            .saturating_add(1)
            .min(MAX_PENDING_INPUT_ECHO_PROMOTIONS);
    }

    /// Take one queued echo promotion, if any.
    ///
    /// The COUNT exists because a fast burst admits several keystrokes before
    /// the first return chunk arrives: consuming membership instead would
    /// promote only the first echo and make every later one wait out
    /// [`super::emit::CELL_EMIT_COALESCE_MS`].
    pub fn consume_input_echo_promotion(&mut self, channel_id: ChannelId) -> bool {
        match self.input_echo.get_mut(&channel_id) {
            Some(queued) if *queued > 0 => {
                *queued -= 1;
                if *queued == 0 {
                    self.input_echo.remove(&channel_id);
                }
                true
            }
            _ => false,
        }
    }

    /// Retire a channel's queued work: the dirty mark, the echo promotions and
    /// any hold. A replaced generation must not have its timer fire against the
    /// grid that replaced it.
    pub fn cancel(&mut self, channel_id: ChannelId) {
        self.dirty.remove(&channel_id);
        self.input_echo.remove(&channel_id);
        self.gates.remove(&channel_id);
    }
}
