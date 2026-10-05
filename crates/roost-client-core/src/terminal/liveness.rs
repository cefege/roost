//! The foreground liveness watchdog: a pane that stopped painting is probed, and
//! a challenge nothing answers escalates.
//!
//! Two deadlines, both armed here and both FIRED BY THE SWEEP — this crate has
//! no timer, so `ClientEvent::Sweep` is the only instant at which either can
//! come due. The quiet deadline runs `TERMINAL_FOREGROUND_IDLE_PROBE_MS` after
//! the last frame an active, visible view accepted; the proof deadline runs
//! `TERMINAL_FOREGROUND_PROBE_DEADLINE_MS` after the challenge that asked for
//! proof went out.
//!
//! The rule every exit obeys: a pane that stopped painting has no other
//! watchdog, so no path may leave it holding neither a probe nor a proof
//! deadline. A challenge that could not be published re-anchors at the CURRENT
//! instant — reusing a stale frame timestamp would arm a deadline that is
//! already past, and the retry would spin.
//!
//! The retry is reported as an EPISODE, never per retry: a warn line every five
//! seconds for one stuck pane would leave the always-on channel permanently red.
//! `rearm_reported` carries that edge, and everything that ends an episode clears
//! it — a published challenge AND retirement — because a flag surviving
//! retirement silences the FIRST rearm of the next episode, which is the only
//! one that gets reported.
//!
//! `session_liveness` is the same rule as hooks on the replica that owns it.
//!
//! Ported from `apps/web/src/store/terminal-stream-repair.ts`
//! (`armTerminalIdleProbeSince`, `armTerminalProofDeadline`,
//! `requestTerminalLivenessChallenge`) and `terminal-stream-liveness.ts`
//! (`clearTerminalSessionLiveness`). The rule this file exists to hold is
//! `docs/FAILURE-INDEX.md` "The liveness watchdog deletes itself on the failure
//! it exists to notice".

use roost_protocol::viewport::{
    TERMINAL_FOREGROUND_IDLE_PROBE_MS, TERMINAL_FOREGROUND_PROBE_DEADLINE_MS,
};

use crate::terminal::idle_probe::IdleProbe;
use crate::terminal::token::TerminalToken;

/// Where one replica's liveness repair ladder stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepairOutcome {
    /// Nothing has been asked for on this replica's behalf.
    #[default]
    None,
    /// A challenge went out and nothing has answered it yet.
    Requested,
    /// A later frame on the challenged stream proved the lane.
    Proved,
    /// The proof deadline expired; the Sync generation is being replaced.
    Escalated,
    /// Nothing is viewed, so nothing is owed.
    Inactive,
    /// The carrier generation changed; the challenge named a socket that is gone.
    GenerationReset,
    /// The authority replaced the stream; the challenge named the one before it.
    StreamReplaced,
    /// The view that would have answered is gone.
    Disposed,
}

impl RepairOutcome {
    /// The diagnostic spelling, which is v2's `TerminalRepairOutcome` verbatim for
    /// every value this client can produce.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Requested => "requested",
            Self::Proved => "proved",
            Self::Escalated => "escalated",
            Self::Inactive => "inactive",
            Self::GenerationReset => "generation_reset",
            Self::StreamReplaced => "stream_replaced",
            Self::Disposed => "disposed",
        }
    }
}

/// What a stale foreground pane is reported as doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallAction {
    /// A challenge went out.
    Resync,
    /// The proof deadline expired and the generation is being replaced.
    Redial,
    /// A challenge could not be published and the probe re-armed.
    Rearm,
}

impl StallAction {
    /// The wire spelling, which is what `roost doctor` groups on.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resync => "resync",
            Self::Redial => "redial",
            Self::Rearm => "rearm",
        }
    }
}

/// One replica's foreground liveness: what is owed, when, and what answered.
#[derive(Debug, Clone, Default)]
pub struct ForegroundLiveness {
    /// When the quiet probe comes due, or `None` with no probe armed.
    quiet_due_ms: Option<u64>,
    /// The generation that armed the probe. A frame from any other generation is
    /// not an answer to it.
    owner: Option<TerminalToken>,
    /// When the proof comes due, or `None` with no challenge outstanding.
    proof_due_ms: Option<u64>,
    /// When the outstanding challenge was issued.
    challenged_at_ms: Option<u64>,
    /// The generation the outstanding challenge names.
    challenge_generation: Option<TerminalToken>,
    /// The stream the outstanding challenge named.
    challenge_stream_id: Option<String>,
    /// The checkpoint the outstanding challenge named. Only a frame PAST this
    /// sequence proves the lane; re-stating the checkpoint proves nothing.
    challenge_seq: Option<u64>,
    /// How many challenges this replica has issued since its last retirement.
    repair_attempts: u32,
    /// What the last transition concluded.
    outcome: RepairOutcome,
    /// Whether this unpublishable-challenge episode has already been reported.
    rearm_reported: bool,
    /// When the last frame this replica ACCEPTED arrived, or `None` since its
    /// last retirement. Distinct from the probe's deadline because a parked pane
    /// stops accepting frames without ever having been watched.
    last_accepted_at_ms: Option<u64>,
    /// The armed probe's anchor and the idle backoff its interval follows.
    idle_probe: IdleProbe,
}

impl ForegroundLiveness {
    /// A replica that has armed nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// When the quiet probe comes due.
    pub fn quiet_due_ms(&self) -> Option<u64> {
        self.quiet_due_ms
    }

    /// When the last frame this replica accepted arrived.
    pub fn last_accepted_at_ms(&self) -> Option<u64> {
        self.last_accepted_at_ms
    }

    /// When the proof comes due.
    pub fn proof_due_ms(&self) -> Option<u64> {
        self.proof_due_ms
    }

    /// The generation that armed the probe.
    pub fn owner(&self) -> Option<&TerminalToken> {
        self.owner.as_ref()
    }

    /// When the outstanding challenge was issued.
    pub fn challenged_at_ms(&self) -> Option<u64> {
        self.challenged_at_ms
    }

    /// The generation the outstanding challenge names.
    pub fn challenge_generation(&self) -> Option<&TerminalToken> {
        self.challenge_generation.as_ref()
    }

    /// The stream the outstanding challenge named.
    pub fn challenge_stream_id(&self) -> Option<&str> {
        self.challenge_stream_id.as_deref()
    }

    /// The checkpoint the outstanding challenge named.
    pub fn challenge_seq(&self) -> Option<u64> {
        self.challenge_seq
    }

    /// How many challenges this replica has issued since its last retirement.
    pub fn repair_attempts(&self) -> u32 {
        self.repair_attempts
    }

    /// What the last transition concluded.
    pub fn outcome(&self) -> RepairOutcome {
        self.outcome
    }

    /// Whether this replica's quiet probe is due.
    pub fn quiet_expiry_due(&self, now_ms: u64) -> bool {
        self.quiet_due_ms.is_some_and(|due| now_ms >= due)
    }

    /// Whether this replica's proof deadline is due.
    pub fn proof_expiry_due(&self, now_ms: u64) -> bool {
        self.proof_due_ms.is_some_and(|due| now_ms >= due)
    }

    /// Anchor the quiet probe at `anchor_ms` — the instant of the last frame this
    /// replica accepted — for the backed-off idle interval.
    ///
    /// Re-anchoring on every frame is what makes the probe a statement about
    /// SILENCE rather than about elapsed time: a pane printing steadily pushes
    /// its own deadline out and is never probed, and only a pane that stopped
    /// painting ever reaches it.
    pub fn arm_quiet(&mut self, owner: &TerminalToken, anchor_ms: u64) {
        self.owner = Some(owner.clone());
        self.last_accepted_at_ms = Some(anchor_ms);
        let interval = self.idle_probe.backed_off_interval_ms();
        self.quiet_due_ms = Some(self.idle_probe.anchor(anchor_ms, interval));
    }

    /// Anchor the quiet probe at `now_ms`, but only with nothing armed.
    ///
    /// The sweep offers this on every pass, so an arm that EXTENDED would push
    /// the deadline out one sweep at a time and the probe could never come due.
    pub fn arm_quiet_if_unarmed(&mut self, owner: &TerminalToken, now_ms: u64) {
        if self.quiet_due_ms.is_some() {
            return;
        }
        self.rearm_quiet(owner, now_ms);
    }

    /// Clear the quiet probe, returning the instant it was anchored at.
    ///
    /// The sweep takes the probe the instant it comes due: what happens next —
    /// a published challenge, or a re-arm anchored at now — owns the session
    /// from there, and a probe left armed fires again over the top of it. The
    /// anchor comes back rather than staying behind so the lateness a re-arm
    /// reports is measured against the last frame that actually painted.
    pub fn take_quiet(&mut self) -> Option<u64> {
        self.quiet_due_ms = None;
        self.idle_probe.take_anchor()
    }

    /// Re-arm the quiet probe from `now_ms`, replacing whatever was armed.
    ///
    /// The anchor is the CURRENT instant and never a frame timestamp: a
    /// challenge that could not be published would otherwise be retried with no
    /// delay at all, once per sweep, for as long as the pane stayed stuck.
    pub fn rearm_quiet(&mut self, owner: &TerminalToken, now_ms: u64) {
        self.owner = Some(owner.clone());
        let due = self
            .idle_probe
            .anchor(now_ms, TERMINAL_FOREGROUND_IDLE_PROBE_MS);
        self.quiet_due_ms = Some(due);
    }

    /// Whether a challenge this generation issued is still outstanding. It is
    /// the sole coalescer: the probe, the repair request and the escalation all
    /// read it, so one gap is one challenge.
    pub fn has_pending_challenge(&self, owner: &TerminalToken) -> bool {
        self.challenged_at_ms.is_some() && self.challenge_generation.as_ref() == Some(owner)
    }

    /// Record a challenge going out, and arm the proof that owes an answer.
    ///
    /// Publishing ends the unpublishable-challenge episode: the edge that
    /// silences its retries is the edge that says the last one got out, and
    /// leaving it set would silence the FIRST rearm of the NEXT episode — the
    /// only one that gets reported.
    pub fn begin_challenge(
        &mut self,
        owner: &TerminalToken,
        stream_id: String,
        seq: u64,
        now_ms: u64,
    ) {
        self.challenged_at_ms = Some(now_ms);
        self.challenge_generation = Some(owner.clone());
        self.challenge_stream_id = Some(stream_id);
        self.challenge_seq = Some(seq);
        self.repair_attempts = self.repair_attempts.saturating_add(1);
        self.outcome = RepairOutcome::Requested;
        self.end_episode();
        self.arm_proof(now_ms);
    }

    /// Re-anchor an outstanding challenge and its proof, for a second request on
    /// the same gap. A request with nothing outstanding arms nothing.
    pub fn rearm_proof(&mut self, now_ms: u64) {
        if self.challenged_at_ms.is_none() {
            return;
        }
        self.challenged_at_ms = Some(now_ms);
        self.repair_attempts = self.repair_attempts.saturating_add(1);
        self.outcome = RepairOutcome::Requested;
        self.arm_proof(now_ms);
    }

    /// Whether a frame would prove the outstanding challenge: the same
    /// generation, the same stream, and a sequence PAST the challenged
    /// checkpoint.
    ///
    /// Separate from the mutation so a caller holding a borrow of the frame's
    /// stream id can ask before it takes the replica.
    pub fn proves(&self, owner: &TerminalToken, stream_id: &str, seq: u64) -> bool {
        self.has_pending_challenge(owner)
            && self.challenge_stream_id.as_deref() == Some(stream_id)
            && seq > self.challenge_seq.unwrap_or_default()
    }

    /// The frame proved the lane: the challenge is answered and the deadline it
    /// owed is discharged, and the next probe of an idle pane waits longer.
    pub fn note_proved(&mut self) {
        self.clear_challenge();
        self.idle_probe.note_proved();
        self.outcome = RepairOutcome::Proved;
    }

    /// A frame that proved nothing arrived: the pane is producing output, so
    /// the next silence is probed at the base interval again.
    pub fn note_output(&mut self) {
        self.idle_probe.note_output();
    }

    /// Challenges answered in a row by their proof alone.
    pub fn proved_idle_streak(&self) -> u32 {
        self.idle_probe.proved_idle_streak()
    }

    /// Whether an arriving chunk part is one this challenge is waiting on.
    pub fn chunk_arrives(&self, owner: &TerminalToken, stream_id: &str, seq: u64) -> bool {
        self.has_pending_challenge(owner)
            && self.challenge_stream_id.as_deref() == Some(stream_id)
            && seq >= self.challenge_seq.unwrap_or_default()
    }

    /// Clear the proof deadline without answering the challenge.
    ///
    /// Two callers, one meaning: the gap between two parts of a baseline is not
    /// silence, and a due deadline with no challenge behind it is owed nothing.
    /// The challenge itself stays in both cases — only a complete frame proves
    /// it.
    pub fn clear_proof(&mut self) {
        self.proof_due_ms = None;
    }

    /// The proof deadline came due and the generation is being replaced.
    ///
    /// The challenge record goes with it. It is spent: leaving it in place would
    /// make the next generation's probe read a challenge against the stream it
    /// replaced, retire against that difference, and re-arm — a churn loop with
    /// no request in it. The generation that owns the next probe is decided when
    /// it is armed, not by a record the old socket left behind.
    pub fn mark_escalated(&mut self) {
        self.clear_challenge();
        self.outcome = RepairOutcome::Escalated;
    }

    /// Whether this episode has not been reported yet, recording that it now
    /// has been. The caller reports exactly one line per episode rather than
    /// one per retry.
    pub fn claim_rearm_episode(&mut self) -> bool {
        if self.rearm_reported {
            return false;
        }
        self.rearm_reported = true;
        true
    }

    /// End the current episode. Retirement ends it as surely as a published
    /// challenge does: a flag surviving here would silence the FIRST rearm of
    /// the next episode, which is the only one that gets reported.
    pub fn end_episode(&mut self) {
        self.rearm_reported = false;
    }

    /// Retire every deadline this replica holds. `outcome` is why, which is the
    /// diagnostic's last word until the next transition.
    pub fn retire(&mut self, outcome: RepairOutcome) {
        self.quiet_due_ms = None;
        self.owner = None;
        self.last_accepted_at_ms = None;
        self.idle_probe.reset();
        self.clear_challenge();
        self.repair_attempts = 0;
        self.outcome = outcome;
        self.end_episode();
    }

    fn arm_proof(&mut self, challenged_at_ms: u64) {
        self.proof_due_ms =
            Some(challenged_at_ms.saturating_add(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS));
    }

    fn clear_challenge(&mut self) {
        self.proof_due_ms = None;
        self.challenged_at_ms = None;
        self.challenge_generation = None;
        self.challenge_stream_id = None;
        self.challenge_seq = None;
    }
}
