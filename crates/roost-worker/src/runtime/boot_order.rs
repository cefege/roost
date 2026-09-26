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
        because: "a surviving keeper is adopted or proved empty before any session is touched, because the channels it holds are live terminals and the decision to end them cannot be taken after the worker has started using them",
    },
    BootStep {
        name: "coordinator-link",
        because: "the link runs after the keeper rather than before it, because the keeper is what holds the terminals and a coordinator outage must not cost a worker its PTYs",
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

    /// Record a step, and return the reason it is where it is so the caller can
    /// log the two together.
    pub fn complete(&mut self, step: StepId) -> &'static str {
        self.completed.push(step);
        BOOT_ORDER[step as usize].because
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
