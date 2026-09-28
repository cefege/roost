//! The vocabulary of direct-terminal grant leases: the non-secret snapshot
//! signaling may inspect, the invalidations subscribers hear, the pending
//! refresh record, the bounded refusals, and the tuple key a lease lives under.
//! Holds no registry; `grant_owner` and `grant_refresh` own every mutation.
//! Ports `apps/coord/src/terminal/direct/terminal-grant-owner-state.ts` and the
//! exported types and constants of `terminal-grant-owner.ts`.

use std::pin::Pin;
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use tokio::sync::watch;

use crate::coord_core::worker_handle::WorkerHandle;

/// The browser credential's lifetime, which is also the coordinator's lease
/// bookkeeping lifetime; workers enforce their own copy.
pub const LOCAL_TERMINAL_GRANT_TTL_MS: u32 = 12 * 60 * 60_000;

/// At most this many owner/tab/worker tuples may be mid-install at once.
pub(crate) const MAX_PENDING_TERMINAL_GRANT_REFRESHES: usize = 256;

/// Why a worker's direct transport is retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalDirectRetireReason {
    /// An operator deleted the machine.
    WorkerDeleted,
    /// The worker's credential was revoked.
    WorkerRevoked,
}

impl TerminalDirectRetireReason {
    /// The wire spelling the worker's retirement frame carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkerDeleted => "worker_deleted",
            Self::WorkerRevoked => "worker_revoked",
        }
    }
}

/// What ended or narrowed a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalGrantInvalidationKind {
    /// The coordinator's bookkeeping lifetime ran out.
    GrantExpired,
    /// The browser device was revoked.
    DeviceRevoked,
    /// A new worker epoch minted a new grant for the same tuple.
    GrantReplaced,
    /// A renewal on the same epoch dropped sessions.
    ScopeReduced,
    /// The worker was deleted or its credential revoked.
    WorkerRetired,
    /// The owner shut down.
    Disposed,
}

impl TerminalGrantInvalidationKind {
    /// The v2 spelling, for log lines and cancellation reasons.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GrantExpired => "grant_expired",
            Self::DeviceRevoked => "device_revoked",
            Self::GrantReplaced => "grant_replaced",
            Self::ScopeReduced => "scope_reduced",
            Self::WorkerRetired => "worker_retired",
            Self::Disposed => "disposed",
        }
    }
}

/// The non-secret lease data signaling may inspect after authenticating a
/// caller. Never carries the secret or its digest.
#[derive(Debug, Clone)]
pub struct TerminalGrantLeaseSnapshot {
    /// The stable grant id.
    pub grant_id: String,
    /// The authenticated owner (account and device) the lease belongs to.
    pub owner_key: String,
    /// The browser device.
    pub device_fingerprint: String,
    /// The browser document.
    pub tab_id: String,
    /// The worker, as the browser named it.
    pub worker_fp: String,
    /// The worker process epoch the grant was installed on; `None` for a
    /// worker that reports none.
    pub worker_epoch: Option<String>,
    /// The sessions the grant covers, in install order.
    pub session_ids: Vec<String>,
    /// When the coordinator forgets the lease, in epoch milliseconds.
    pub expires_at_ms: i64,
    /// The exact current worker generation allowed to carry this grant.
    pub worker_handle: Arc<WorkerHandle>,
}

/// One change subscribers hear about.
#[derive(Debug, Clone)]
pub struct TerminalGrantInvalidation {
    /// What happened.
    pub kind: TerminalGrantInvalidationKind,
    /// The lease it happened to; `None` for a retirement that found none.
    pub lease: Option<TerminalGrantLeaseSnapshot>,
    /// The worker it concerns.
    pub worker_fp: String,
    /// That worker's epoch, when known.
    pub worker_epoch: Option<String>,
    /// The device it concerns, when it concerns one.
    pub device_fingerprint: Option<String>,
    /// The sessions that lost authority.
    pub removed_session_ids: Vec<String>,
    /// The retirement reason, for a retirement.
    pub reason: Option<TerminalDirectRetireReason>,
}

impl TerminalGrantInvalidation {
    /// The invalidation of one whole lease.
    #[must_use]
    pub fn of_lease(
        kind: TerminalGrantInvalidationKind,
        lease: &TerminalGrantLeaseSnapshot,
        removed_session_ids: Vec<String>,
        reason: Option<TerminalDirectRetireReason>,
    ) -> Self {
        Self {
            kind,
            lease: Some(lease.clone()),
            worker_fp: lease.worker_fp.clone(),
            worker_epoch: lease.worker_epoch.clone(),
            device_fingerprint: Some(lease.device_fingerprint.clone()),
            removed_session_ids,
            reason,
        }
    }
}

/// A subscriber to lease invalidations.
pub type InvalidationListener = Arc<dyn Fn(&TerminalGrantInvalidation) + Send + Sync>;

/// A durable route-authority check, re-run before and after each install.
pub type AuthorizationFuture = Pin<Box<dyn Future<Output = Result<(), ConnectError>> + Send>>;

/// Re-runs route authority for the session union a refresh will install.
pub type TerminalGrantAuthorization = Arc<dyn Fn(Vec<String>) -> AuthorizationFuture + Send + Sync>;

/// One browser's demand for a grant.
#[derive(Clone)]
pub struct TerminalGrantRequest {
    /// The authenticated owner key.
    pub owner_key: String,
    /// The browser device.
    pub device_fingerprint: String,
    /// The browser document.
    pub tab_id: String,
    /// The worker, as the browser named it.
    pub worker_fp: String,
    /// The sessions the browser needs covered.
    pub session_ids: Vec<String>,
    /// Durable route authority for the union this refresh installs.
    pub authorize: TerminalGrantAuthorization,
}

impl std::fmt::Debug for TerminalGrantRequest {
    /// Everything but the authorization closure, which has no useful rendering.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalGrantRequest")
            .field("tab_id", &self.tab_id)
            .field("worker_fp", &self.worker_fp)
            .field("session_ids", &self.session_ids)
            .finish_non_exhaustive()
    }
}

/// A committed lease and the browser secret it was installed with.
#[derive(Debug, Clone)]
pub struct TerminalGrantResult {
    /// The lease as committed.
    pub lease: TerminalGrantLeaseSnapshot,
    /// The browser secret; the worker only ever saw its digest.
    pub secret: String,
}

/// What every waiter on one refresh eventually reads.
pub(crate) type RefreshOutcome = Option<Result<TerminalGrantResult, ConnectError>>;

/// The one bounded install in flight for an owner/tab/worker tuple.
pub(crate) struct PendingGrantRefresh {
    /// Distinguishes this refresh from a later one under the same key.
    pub(crate) id: u64,
    pub(crate) owner_key: String,
    pub(crate) device_fingerprint: String,
    pub(crate) tab_id: String,
    pub(crate) worker_fp: String,
    /// The coalesced demand, a set kept in arrival order.
    pub(crate) session_ids: Vec<String>,
    /// The most recent caller's authority check.
    pub(crate) authorize: TerminalGrantAuthorization,
    /// Set by a revocation, retirement or shutdown; the refresh then fails its
    /// next liveness check instead of committing.
    pub(crate) invalidated: bool,
    /// Where every coalesced caller reads the one outcome.
    pub(crate) outcome: Arc<watch::Sender<RefreshOutcome>>,
}

/// The owner/tab/worker tuple one lease and one refresh live under.
pub(crate) type LeaseKey = (String, String, String);

/// The tuple key, as a value rather than v2's JSON string.
pub(crate) fn lease_key(owner_key: &str, tab_id: &str, worker_fp: &str) -> LeaseKey {
    (
        owner_key.to_owned(),
        tab_id.to_owned(),
        worker_fp.to_owned(),
    )
}

/// A demand outside the one-to-256 session bound.
pub(crate) fn invalid_grant_sessions() -> ConnectError {
    ConnectError::new(
        ErrorCode::InvalidArgument,
        "terminal grant sessions are invalid",
    )
}

/// No room for another refresh, or for the grown union.
pub(crate) fn grant_capacity_exceeded() -> ConnectError {
    ConnectError::new(
        ErrorCode::ResourceExhausted,
        "terminal grant refresh capacity is exhausted",
    )
}

/// The worker generation the grant needs is gone or changed.
pub(crate) fn worker_unavailable() -> ConnectError {
    ConnectError::new(ErrorCode::Unavailable, "worker unavailable")
}

/// The owner shut down.
pub(crate) fn owner_disposed() -> ConnectError {
    ConnectError::new(ErrorCode::Unavailable, "terminal grant owner disposed")
}
