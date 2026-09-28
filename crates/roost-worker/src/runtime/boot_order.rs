//! The order boot runs in, why each step is where it is, and the one ordering
//! rule a worker may not break. Called by `serve` as it runs, and by nothing
//! else.
//!
//! Two artifacts, for two different reasons. [`BootSequence`] is the checklist
//! `serve` drives itself from, so the order is written down once and executed
//! in that order rather than emerging from the shape of the code. [`Readiness`]
//! is the rule underneath it, and it is a state machine because the rule is
//! about ordering: reconciliation must reserve every durable session before a
//! snapshot publishes, so a failed keeper adoption cannot expose a partial
//! worker state. That is the reason v2 put all three steps in one function
//! (`apps/worker/src/boot/worker-boot-admission.ts`) rather than letting the
//! caller sequence them, and it is why the rule is enforced here instead of
//! trusted.

/// One step of boot, and the invariant that puts it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootStep {
    /// What the step is called, in the log.
    pub name: &'static str,
    /// Why it happens here and not earlier or later.
    pub because: &'static str,
}

/// The order, as declared. Every reason here is a refusal that happens if the
/// step moves.
pub const BOOT_ORDER: [BootStep; 5] = [
    BootStep {
        name: "identity",
        because: "the fingerprint and the coordinator are settled before anything is probed or spawned, because a keeper mutation made under the wrong identity is a mutation against another worker's sessions",
    },
    BootStep {
        name: "keeper-admission",
        because: "the keeper is admitted BEFORE the link is built, because the survivor decision reads the coordinator's open-session set over Connect and that read needs no socket of ours, while the link object dispatches browser commands into the session layer that is built over the keeper — so the link cannot exist before the keeper, and a machine that replaced its keeper on an unread set would end somebody's terminal",
    },
    BootStep {
        name: "coordinator-link",
        because: "the link is recorded after the keeper and its DIAL is last of all, because the object is built over the session layer and the session layer over the pool, and the dial is `link.run` at the end of boot — the previous text here claimed the link DIALS before the keeper is admitted, which was false and sent an incident reader to the wrong place",
    },
    BootStep {
        name: "session-reconcile",
        because: "the coordinator's complete open-session set is reserved before snapshots publish, because a snapshot that omits a live session is the coordinator closing it",
    },
    BootStep {
        name: "ready",
        because: "readiness is announced last, because readiness is a claim about the two steps above and announcing it earlier is a claim this process cannot back",
    },
];

/// Which step of [`BOOT_ORDER`] is being recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepId {
    Identity,
    KeeperAdmission,
    CoordinatorLink,
    SessionReconcile,
    Ready,
}

impl StepId {
    /// Every step, in the order [`BOOT_ORDER`] declares them.
    ///
    /// The enum and the array are two artifacts that must move together, and
    /// nothing in the type system ties them: `name()` reads the array by
    /// `step as usize`, so reordering one without the other renames every boot
    /// step while every log line still looks plausible. This list is what lets
    /// a test walk the pair and say which one moved.
    ///
    /// IT DOES NOT KNOW THE RIGHT ORDER. Agreement between the two only says
    /// they agree; a swap that moves both is still a swap, and this list moves
    /// with it. The oracle for correctness is the name vector in
    /// `tests/worker_boot_order.rs`, and a reorder has to change that
    /// deliberately.
    pub const ALL: [StepId; 5] = [
        StepId::Identity,
        StepId::KeeperAdmission,
        StepId::CoordinatorLink,
        StepId::SessionReconcile,
        StepId::Ready,
    ];
    /// The step's name, as the log and the checklist spell it.
    pub fn name(self) -> &'static str {
        match self {
            StepId::Identity => BOOT_ORDER[0].name,
            StepId::KeeperAdmission => BOOT_ORDER[1].name,
            StepId::CoordinatorLink => BOOT_ORDER[2].name,
            StepId::SessionReconcile => BOOT_ORDER[3].name,
            StepId::Ready => BOOT_ORDER[4].name,
        }
    }
}

/// What boot has completed, in the order it completed it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootSequence {
    completed: Vec<StepId>,
}

impl BootSequence {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{} cannot be recorded while boot has recorded {recorded:?}", step.name())]
pub struct StepOutOfOrder {
    /// The step that was asked for.
    pub step: StepId,
    /// What had been recorded when it was asked for, in order.
    pub recorded: Vec<StepId>,
}

impl StepOutOfOrder {
    /// The names of what was already recorded, in order.
    pub fn recorded_names(&self) -> Vec<&'static str> {
        self.recorded.iter().map(|step| step.name()).collect()
    }
}

impl BootSequence {
    /// Record a step, and return the reason it is where it is so the caller can
    /// log the two together.
    ///
    /// **THIS REFUSES AN OUT-OF-ORDER STEP, and refusing is the whole point of
    /// F5.** It accepted any order until now, which is why the declaration and
    /// the execution could disagree for so long with nothing noticing: the log
    /// printed whatever sequence it was given, and the declaration beside it
    /// asserted a different one. A boot that records `coordinator-link` before
    /// `keeper-admission` is a boot whose own log contradicts its own
    /// architecture, and a refusal is the only answer that stops it being
    /// written.
    ///
    /// The rule is `StepId::ALL` POSITIONALLY, so it is the same array the
    /// declaration is, and a step is legal exactly when it is the one after
    /// everything already recorded. Re-recording a step is refused too: a
    /// second `ready` is not a stricter boot, it is a different one.
    pub fn complete(&mut self, step: StepId) -> Result<&'static str, StepOutOfOrder> {
        let expected = self.completed.len();
        if StepId::ALL.get(expected) != Some(&step) {
            return Err(StepOutOfOrder {
                step,
                recorded: self.completed.clone(),
            });
        }
        self.completed.push(step);
        Ok(BOOT_ORDER[step as usize].because)
    }
    /// The steps completed, in order.
    pub fn completed(&self) -> &[StepId] {
        &self.completed
    }
}

/// The three things that must happen, in this order, before a worker is ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyStep {
    /// The coordinator's open-session set is reserved and the local set matches.
    Reconciled,
    /// The snapshot provider is live, so the coordinator can be told everything.
    SnapshotProviderActivated,
    /// Readiness is announced.
    MarkedReady,
}

impl ReadyStep {
    pub fn name(self) -> &'static str {
        match self {
            ReadyStep::Reconciled => "session-reconcile",
            ReadyStep::SnapshotProviderActivated => "snapshot-provider",
            ReadyStep::MarkedReady => "ready",
        }
    }
}

/// How far boot has got towards being ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Readiness {
    #[default]
    Starting,
    Reconciled,
    SnapshotActive,
    Ready,
}

/// A step asked for in a position the order does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{} cannot run while boot is {at:?}", step.name())]
pub struct OutOfOrderStep {
    /// The step that was asked for.
    pub step: ReadyStep,
    /// Where boot actually was.
    pub at: Readiness,
}

impl Readiness {
    /// Advance, or refuse.
    ///
    /// Refusing rather than tolerating is the point: a caller that activates the
    /// snapshot provider before reconciling produces a snapshot missing every
    /// session the coordinator still lists as open, and the coordinator acts on
    /// it. That failure is silent and it closes terminals, so it gets an error
    /// here instead of a comment at the call site.
    pub fn advance(&mut self, step: ReadyStep) -> Result<Self, OutOfOrderStep> {
        // Bound BEFORE the match, which moves the receiver: the error arm has
        // to name the state the step arrived in, and by then `self` is gone.
        let at = *self;
        let next = match (at, step) {
            (Readiness::Starting, ReadyStep::Reconciled) => Readiness::Reconciled,
            (Readiness::Reconciled, ReadyStep::SnapshotProviderActivated) => {
                Readiness::SnapshotActive
            }
            (Readiness::SnapshotActive, ReadyStep::MarkedReady) => Readiness::Ready,
            _ => return Err(OutOfOrderStep { step, at }),
        };
        *self = next;
        Ok(next)
    }

    pub fn is_ready(self) -> bool {
        self == Readiness::Ready
    }
}
