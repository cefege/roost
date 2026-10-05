//! The liveness watchdog as hooks on the replica that owes it.
//!
//! Split out of `liveness` for the same reason `session_views` exists next to
//! `session`: the state machine is one question ("what is this replica owed,
//! and when?") and the hooks are another ("what just happened to it?"). This is
//! what you read when a frame was accepted; `liveness` is what you read when
//! time passed.
//!
//! Every hook here is allocation-free on the frame path: a terminal receives
//! deltas many times a second, and the watchdog must not be the reason a frame
//! costs a `String`. The predicate methods on `ForegroundLiveness` exist so the
//! borrowed stream id dies before the replica is taken mutably.
//!
//! Depends on `liveness`, `token` and `view`; called by `session` on admission
//! and by `handle_sweep` on a transition.

use crate::terminal::liveness::{ForegroundLiveness, RepairOutcome};
use crate::terminal::session::TerminalSession;
use crate::terminal::token::TerminalToken;
use crate::terminal::view::{TerminalView, ViewIntent};

impl TerminalSession {
    /// This replica's foreground liveness, for the sweep and for diagnostics.
    pub fn liveness(&self) -> &ForegroundLiveness {
        &self.liveness
    }

    /// This replica's foreground liveness, for the caller that owns the next
    /// transition.
    pub fn liveness_mut(&mut self) -> &mut ForegroundLiveness {
        &mut self.liveness
    }

    /// The view whose liveness this replica owes: the first one still asking the
    /// authority to show it. A parked pane constrains no geometry and is painting
    /// nothing, so it is not the foreground.
    pub fn foreground_view(&self) -> Option<&TerminalView> {
        self.views
            .values()
            .find(|view| matches!(view.intent, ViewIntent::Publish { .. }))
    }

    /// Retire every deadline this replica holds, for a transition that leaves it
    /// with nothing watched.
    pub fn retire_liveness(&mut self, outcome: RepairOutcome) {
        self.liveness.retire(outcome);
    }

    /// A frame was ACCEPTED: note it, discharge any challenge it proves, and
    /// re-anchor the quiet probe.
    ///
    /// Re-anchoring here rather than at the probe's own expiry is what makes the
    /// probe a statement about SILENCE. v2 anchors at the last accepted frame and
    /// re-checks the same fact when the timer fires; one anchor here is the same
    /// rule expressed once.
    pub fn note_accepted_frame(&mut self, token: &TerminalToken, now_ms: u64) {
        if self.generation() != Some(token) || !self.baseline_ready() {
            return;
        }
        let Some((stream_id, seq)) = self
            .canonical()
            .map(|frame| (frame.stream_id.as_str(), frame.seq))
        else {
            return;
        };
        if self.liveness.proves(token, stream_id, seq) {
            self.liveness.note_proved();
        } else if !self.liveness.has_pending_challenge(token) {
            // A frame while a challenge is pending that does not prove it
            // re-states the checkpoint; it is not output.
            self.liveness.note_output();
        }
        self.arm_quiet_probe(token, now_ms);
    }

    /// A chunk part for this replica arrived: a baseline is still arriving, so a
    /// pending proof must not read the gap between two parts as silence.
    ///
    /// It re-anchors nothing. A part is not a painted frame, and a stalled
    /// transfer belongs to the assembler's deadline and a latched gap rather
    /// than to this watchdog — arming the probe off a part would put a pane
    /// mid-transfer in the same "quiet" bucket as a pane that stopped painting.
    pub fn note_chunk_progress(&mut self, token: &TerminalToken, stream_id: &str, seq: u64) {
        if self.generation() != Some(token) {
            return;
        }
        if self.liveness.chunk_arrives(token, stream_id, seq) {
            self.liveness.clear_proof();
        }
    }

    /// Arm the quiet probe if this replica is one the watchdog watches: a
    /// generation, a complete baseline, and a view the foreground can see.
    pub fn arm_quiet_probe(&mut self, token: &TerminalToken, now_ms: u64) {
        if self.generation() != Some(token)
            || !self.baseline_ready()
            || self.foreground_view().is_none()
        {
            return;
        }
        self.liveness.arm_quiet(token, now_ms);
    }

    /// A scoped repair went out on this replica's behalf — a latched gap or a
    /// challenge — so it owes a proof even when nothing asked for one.
    ///
    /// A second request for a gap already under challenge only re-anchors the
    /// existing one, so concurrent view renewals cannot multiply challenges.
    pub fn begin_scoped_repair(&mut self, owner: &TerminalToken, now_ms: u64) {
        if self.generation() != Some(owner) {
            return;
        }
        let Some(stream_id) = self.expected_stream_id().map(str::to_owned) else {
            // A resync names the stream it is a baseline OF; with no expected
            // stream there was nothing to name and nothing to prove.
            return;
        };
        let seq = self
            .canonical()
            .filter(|frame| frame.stream_id == stream_id)
            .map_or(0, |frame| frame.seq);
        if self.liveness.has_pending_challenge(owner) {
            self.liveness.rearm_proof(now_ms);
            return;
        }
        self.liveness.begin_challenge(owner, stream_id, seq, now_ms);
    }
}
