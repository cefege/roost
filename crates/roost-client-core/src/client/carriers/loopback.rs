//! The loopback probe: whether a loopback carrier is available BEFORE any
//! WebRTC peer is allocated. Owned by `client::carriers`, asked by `Signalling`
//! before every attempt. Until the probe answers, no peer may be allocated — the
//! fast path wins (`smoke/terminal/terminal-peer.spec.ts:61`). Ported from
//! `localWorkerDiscovery.ts` and the gate at `terminal-peer.ts:195`.

/// How long the machine waits for a probe answer before it will consider
/// allocating a peer at all.
///
/// v2 parks in `idle` for this long and comes back, so a slow local door costs a
/// short pause rather than a peer negotiation. Two seconds is the local door's
/// own probe timeout plus the margin a loaded machine needs to answer it.
pub const LOOPBACK_GRACE_MS: u64 = 2_100;

/// A worker's local UI door, as the page discovered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkerDoor {
    /// The worker that serves this door. The fingerprint is what proves the
    /// door is the worker being asked for rather than some other process on the
    /// same box, which is the whole reason a door on a shared machine is
    /// useless without it.
    pub worker_fingerprint: String,
    /// Where to dial it.
    pub url: String,
}

/// What the probe has learned about this page's own machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackAnswer {
    /// No answer yet. A peer MUST NOT be allocated in this state: the fast path
    /// has not been ruled out, and ruling it out costs a grace period.
    Unanswered,
    /// This page is served BY this worker, so loopback is the carrier.
    SameHost,
    /// This page is not on the worker's machine, so a peer is the only direct
    /// carrier available.
    OtherHost,
}

/// One worker's loopback answer, whether a loopback carrier is staged, and the
/// door to dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackProbe {
    worker_fp: String,
    answer: LoopbackAnswer,
    door: Option<LocalWorkerDoor>,
    staged: bool,
}

impl LoopbackProbe {
    /// A probe for one worker with no answer yet.
    pub fn new(worker_fp: impl Into<String>) -> Self {
        Self {
            worker_fp: worker_fp.into(),
            answer: LoopbackAnswer::Unanswered,
            door: None,
            staged: false,
        }
    }

    /// The answer as it stands.
    pub fn answer(&self) -> LoopbackAnswer {
        self.answer
    }

    /// The discovered door, when there is one.
    pub fn door(&self) -> Option<&LocalWorkerDoor> {
        self.door.as_ref()
    }

    /// A loopback carrier for this worker came up, or the one that was is gone.
    ///
    /// The carrier is the loopback slice's own connection and this is only the
    /// election's view of it, so the host reports presence rather than this
    /// module minting an id. It is a boolean and not a count because a worker
    /// has one loopback door: two of them would be two connections to one
    /// process, and only the first to authenticate should be believed.
    pub fn set_staged(&mut self, staged: bool) {
        self.staged = staged;
    }

    /// Whether a loopback carrier exists for this worker, authenticated and
    /// staged.
    ///
    /// This is the question the fault fallback asks, and the answer is what
    /// makes a loopback carrier the fallback rather than a hope: it existed
    /// BEFORE the fault, opened straight from the discovered door, with no
    /// negotiation a fault could have interrupted.
    pub fn has_staged_carrier(&self) -> bool {
        self.staged
    }

    /// Fold in the probe's answer.
    ///
    /// `worker_fp` is the fingerprint of the worker serving this page's own
    /// machine, and EMPTY means "not a worker's machine". Anything else — a
    /// different worker, or a page that cannot be told — is `OtherHost`,
    /// because a loopback carrier to a worker this page is not co-located with
    /// does not exist, and assuming one might is how a peer gets allocated
    /// behind a fast path that was available all along.
    pub fn answered(&mut self, worker_fp: &str) {
        self.answer = if !worker_fp.is_empty() && worker_fp == self.worker_fp {
            LoopbackAnswer::SameHost
        } else {
            LoopbackAnswer::OtherHost
        };
        if self.answer == LoopbackAnswer::OtherHost {
            self.door = None;
        }
    }

    /// Record the dialable door for a worker this page shares a machine with.
    ///
    /// Separate from `answered` because the ANSWER and the ADDRESS are separate
    /// facts: a page knows from its own origin which worker serves it, and only
    /// a host that reached the machine's default port knows where to dial. A
    /// door for another worker, an empty one, or one recorded while the answer
    /// says this page is elsewhere is discarded rather than stored for a later
    /// caller to trust.
    pub fn set_door(&mut self, door: LocalWorkerDoor) {
        if self.answer != LoopbackAnswer::SameHost
            || door.worker_fingerprint != self.worker_fp
            || door.url.is_empty()
        {
            return;
        }
        self.door = Some(door);
    }

    /// Whether a WebRTC peer may be allocated right now.
    ///
    /// The rule this exists for: NO while the answer is `Unanswered`, and NO
    /// while it is `SameHost`. A peer is allocated only once the probe has said
    /// the page is somewhere else. v2 states this as an early return rather than
    /// a predicate, and the difference matters — a predicate is a thing a caller
    /// can be shown to hold.
    pub fn permits_peer(&self) -> bool {
        self.answer == LoopbackAnswer::OtherHost
    }

    /// How long to wait before asking the probe again, given the answer.
    ///
    /// `None` means "settled, do not come back to it": a fast path that exists
    /// and a fast path that does not are both facts, and neither needs
    /// re-asking every grace period. v2 only re-enters the probe check because
    /// it also uses the return to schedule a retry, not because the answer could
    /// have changed.
    pub fn recheck_after_ms(&self) -> Option<u64> {
        match self.answer {
            LoopbackAnswer::Unanswered => Some(LOOPBACK_GRACE_MS),
            LoopbackAnswer::SameHost | LoopbackAnswer::OtherHost => None,
        }
    }
}
