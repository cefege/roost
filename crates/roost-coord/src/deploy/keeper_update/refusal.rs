//! Why a keeper update was refused: the vocabulary the whole decision speaks.
//!
//! Owned by `deploy::keeper_update`, which every other file here returns. The
//! shape follows one rule -- a refusal names the fact that was missing or
//! contradicted, because it is the only thing between an operator and a fleet's
//! live PTYs -- and the code keeps a DECISION apart from a TRANSPORT FAULT.

use connectrpc::{ConnectError, ErrorCode};

/// A keeper update this coordinator will not authorize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperUpdateRefusal {
    /// The named machine is not a fingerprint this coordinator could route to.
    MalformedWorkerFingerprint,
    /// `force_live` destroys live PTYs and is meaningful only on the
    /// maintenance path, which carries no journaled envelope.
    ForceLiveWithoutMaintenance,
    /// A maintenance request that also carries an update: two different asks.
    MaintenanceWithJournal,
    /// A replacement arrived with no envelope to prove what may be replaced.
    JournaledUpdateRequired,
    /// The envelope's direction is neither the source nor the target machine.
    InvalidDirection,
    /// The envelope broke the shared admission contract -- a digest disagreeing
    /// with its own admission, a classification its contracts contradict, or a
    /// `replace-empty` naming anything but the canonical empty binding digest.
    MalformedJournaledUpdate,
    /// The coordinator's own open-session list is not canonical, so the proof
    /// about to be handed the worker is not one it can compare against.
    MalformedOpenSessionProof,
    /// A row this decision rests on could not be read. The row that failed is
    /// named in the log line the site emitted, not here: this is a wiring fault
    /// with no operator remedy, and a caller naming it in the message would
    /// only teach the message to be read past.
    CoordinatorReadFailed,
    /// A maintenance shutdown would destroy PTYs the coordinator can still see.
    MaintenanceBlockedByLiveSessions,
    /// A `replace-empty` while the coordinator can still see PTYs: the keeper
    /// the worker will retire is provably empty, but these are not.
    ReplacementBlockedByLiveSessions,
    /// The machine has no live registry row, or its row is a tombstone.
    WorkerNotFound,
    /// The caller's key is no longer a browser.
    AuthenticationRequired,
    /// Another keeper update already holds the drain. Retryable.
    DrainHeld,
    /// The decision was reached with the drain released. A wiring fault, never
    /// an operator error, and the one refusal that answers "this build is
    /// broken" rather than "this deployment is not admissible".
    DrainNotHeld,
    /// The worker answered without naming an outcome.
    MalformedWorkerProof,
    /// The worker's outcome is not one the requested action permits. The
    /// refuse-closed condition, and the reason the whole slice exists.
    ProofForDifferentAction,
    /// A `preserve` arrived without a well-formed keeper identity, so nobody can
    /// tell which process now holds the PTYs.
    MalformedKeeperIdentity,
    /// A shutdown reported an identity: the worker answered about a different
    /// keeper than the one this call replaced.
    UnexpectedKeeperIdentity,
    /// The machine has no current routable generation.
    WorkerOffline,
}

impl KeeperUpdateRefusal {
    /// The reason, and the code it answers with.
    ///
    /// ONE MATCH, so a refusal cannot grow a message without also growing a
    /// code and a reader never has to hold the two apart. `Unavailable` here is
    /// a worker that never answered or a drain already running, never an
    /// unprovable keeper; `DataLoss` is a proof the coordinator could not place.
    #[must_use]
    pub fn reason(self) -> (&'static str, ErrorCode) {
        use ErrorCode::{
            DataLoss, FailedPrecondition, Internal, InvalidArgument, NotFound, Unavailable,
            Unauthenticated,
        };
        match self {
            Self::MalformedWorkerFingerprint => {
                ("worker fingerprint is invalid", InvalidArgument)
            }
            Self::ForceLiveWithoutMaintenance => {
                ("keeper force-live requires the maintenance path", InvalidArgument)
            }
            Self::MaintenanceWithJournal => {
                ("keeper maintenance cannot carry a journaled update", InvalidArgument)
            }
            Self::JournaledUpdateRequired => {
                ("journaled keeper update is required", InvalidArgument)
            }
            Self::InvalidDirection => ("keeper update direction is invalid", InvalidArgument),
            Self::MalformedJournaledUpdate => {
                ("journaled keeper update is malformed", InvalidArgument)
            }
            Self::MalformedOpenSessionProof => {
                ("coordinator open-session proof is malformed", DataLoss)
            }
            Self::CoordinatorReadFailed => {
                ("coordinator could not read a row this decision rests on", Internal)
            }
            Self::MaintenanceBlockedByLiveSessions => {
                ("keeper maintenance blocked by live sessions", FailedPrecondition)
            }
            Self::ReplacementBlockedByLiveSessions => {
                ("keeper replacement blocked by live sessions", FailedPrecondition)
            }
            Self::WorkerNotFound => ("worker not found", NotFound),
            Self::AuthenticationRequired => ("authentication required", Unauthenticated),
            Self::DrainHeld => ("coordinator keeper update preparation is held", Unavailable),
            Self::DrainNotHeld => (
                "keeper update decided without the exclusive write drain",
                Internal,
            ),
            Self::MalformedWorkerProof => ("worker returned malformed keeper proof", DataLoss),
            Self::ProofForDifferentAction => {
                ("worker returned keeper proof for a different action", DataLoss)
            }
            Self::MalformedKeeperIdentity => {
                ("worker returned malformed keeper identity", DataLoss)
            }
            Self::UnexpectedKeeperIdentity => {
                ("worker returned unexpected keeper identity", DataLoss)
            }
            Self::WorkerOffline => ("worker offline", Unavailable),
        }
    }

    /// The code this refusal answers with.
    #[must_use]
    pub fn code(self) -> ErrorCode {
        self.reason().1
    }
}

impl std::fmt::Display for KeeperUpdateRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason().0)
    }
}

impl From<KeeperUpdateRefusal> for ConnectError {
    fn from(refusal: KeeperUpdateRefusal) -> Self {
        ConnectError::new(refusal.code(), refusal.to_string())
    }
}
