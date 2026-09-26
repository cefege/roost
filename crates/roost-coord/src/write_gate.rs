//! The exclusive drain a keeper update takes, and the leases every other
//! mutation waits behind.
//!
//! Owned by the coordinator; the Connect interceptor leases from it
//! (`apps/coord/src/auth/auth-interceptor.ts:201`) and the worker frame
//! dispatcher checks it before acknowledging
//! (`apps/coord/src/workers/worker-frame-dispatch.ts:122-127`). Exactly one
//! instance exists per process and it is a field on the services struct, never
//! a crate-root static.
//!
//! WHY IT IS EXCLUSIVE AT ALL. A keeper update replaces the binary every live
//! PTY depends on. While the drain runs, an event that acknowledged "this PTY
//! is live" would be lost if the process died, and the worker would never replay
//! it -- so the gate withholds the acknowledgement rather than the write. The
//! comment that states it: "A held keeper-update fence withholds the ACK so
//! CoordLink replays the preserved entry after the update. Acking here loses the
//! durable record of which PTYs are live."
//!
//! WHY TERMINAL WRITES DO NOT TAKE THE EXCLUSIVE LEASE. A user typing must not
//! be able to hold the global drain open behind a per-session FIFO
//! (`auth-interceptor.ts:114-116`): "Terminal writes acquire their lease only
//! after entering the per-sender/session FIFO. Taking one here would let queued
//! input or a prompt hold the exclusive keeper-update drain open." A port that
//! puts `SessionsInput` in the lease list reintroduces exactly that.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Why a lease was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WriteGateError {
    /// An exclusive drain is already running, so a shared mutation must wait.
    #[error("coordinator keeper update preparation in progress")]
    InProgress,
    /// The caller asked for the exclusive drain while one was already held.
    #[error("coordinator keeper update preparation is held")]
    Held,
}

/// The gate's state, in the two booleans that matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteGateState {
    /// An exclusive drain holds the gate.
    pub exclusive_held: bool,
    /// Shared leases are outstanding.
    pub shared_leases: u64,
}

/// One process's write gate.
///
/// `Clone` shares the gate, which is the point: a lease taken in a handler and
/// released in an interceptor `finally` must refer to the same counter even
/// though they live in different frames.
#[derive(Debug, Clone, Default)]
pub struct WriteGate {
    inner: Arc<WriteGateInner>,
}

#[derive(Debug, Default)]
struct WriteGateInner {
    exclusive: AtomicBool,
    shared: AtomicU64,
}

impl WriteGate {
    /// A gate with nothing held.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an exclusive drain is running right now.
    ///
    /// Checked on the worker's event path, where the answer decides whether to
    /// withhold an acknowledgement.
    #[must_use]
    pub fn exclusive_held(&self) -> bool {
        self.inner.exclusive.load(Ordering::Acquire)
    }

    /// The current state, for a log line or a test.
    #[must_use]
    pub fn state(&self) -> WriteGateState {
        WriteGateState {
            exclusive_held: self.exclusive_held(),
            shared_leases: self.inner.shared.load(Ordering::Acquire),
        }
    }

    /// Take the exclusive drain, failing if one is already running.
    ///
    /// The gate is one-way until [`WriteGate::release_exclusive`]: there is no
    /// queue of waiters, because a second keeper update arriving mid-drain is a
    /// caller error and making it wait would only extend the window in which
    /// every other mutation is blocked.
    pub fn acquire_exclusive(&self) -> Result<ExclusiveDrain, WriteGateError> {
        if self
            .inner
            .exclusive
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(WriteGateError::Held);
        }
        Ok(ExclusiveDrain { gate: self.clone() })
    }

    /// Take a shared lease, failing while an exclusive drain runs.
    pub fn acquire_shared(&self) -> Result<SharedLease, WriteGateError> {
        if self.exclusive_held() {
            return Err(WriteGateError::InProgress);
        }
        self.inner.shared.fetch_add(1, Ordering::AcqRel);
        if self.exclusive_held() {
            // The exclusive drain started between the check and the increment.
            // Give the lease straight back rather than admitting a writer that
            // began after the drain did: the ordering that matters is "no shared
            // lease outlives the start of a drain", and only the re-check and
            // the release together establish it.
            self.release_shared();
            return Err(WriteGateError::InProgress);
        }
        Ok(SharedLease { gate: self.clone() })
    }

    fn release_shared(&self) {
        // A saturating decrement: an underflow here would wrap to u64::MAX and
        // wedge the gate closed for the life of the process, which is the one
        // failure mode a counter must never have.
        let _ = self
            .inner
            .shared
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                Some(count.saturating_sub(1))
            });
    }

    fn release_exclusive(&self) {
        self.inner.exclusive.store(false, Ordering::Release);
    }
}

/// The exclusive drain, released on drop.
///
/// `Drop` rather than an explicit release so a panicking handler cannot leave
/// the gate closed: a gate that never reopens blocks every mutation in the
/// process, and the process has no other way to notice.
#[derive(Debug)]
pub struct ExclusiveDrain {
    gate: WriteGate,
}

impl Drop for ExclusiveDrain {
    fn drop(&mut self) {
        self.gate.release_exclusive();
    }
}

/// One shared mutation's lease, released on drop.
#[derive(Debug)]
pub struct SharedLease {
    gate: WriteGate,
}

impl Drop for SharedLease {
    fn drop(&mut self) {
        self.gate.release_shared();
    }
}

/// The RPC methods that take a shared lease, spelled as the **proto** method
/// names because that is the string both the interceptor and the audit policy
/// key on (`apps/coord/src/auth/auth-interceptor.ts:111-132`).
///
/// A method absent from this list is a durable mutation running unfenced; a
/// method wrongly present is a read that can fail during a keeper update. The
/// list is spelled out rather than derived from a naming convention because a
/// convention guesses wrong in both directions: `SessionsSearchScrollback` and
/// `UiReportState` look like writes and are not, and `WorkersHeartbeat` looks
/// like a read and is a durable write.
///
/// Two deliberate absences, both called out in the source: `SessionsInput` and
/// `SessionsPrompt` are **not** here, and the reason is in the module header.
/// `SessionsCursorPos` **is**, sitting directly under that comment.
#[must_use]
pub fn method_holds_lease(method: &str) -> bool {
    matches!(
        method,
        // Worker lifecycle. A heartbeat is a durable write, so it is fenced
        // like the rest even though it reads like a read.
        "WorkersRegister"
            | "WorkersHeartbeat"
            | "WorkersRename"
            | "WorkersDelete"
            | "WorkersDeployStart"
        // Session lifecycle.
            | "SessionsSpawn"
            | "SessionsAttach"
            | "SessionsKill"
            | "SessionsRename"
            | "SessionsCursorPos"
            | "SessionsAssignWorkspace"
        // Tasks.
            | "TasksEnqueue"
            | "TasksNextPending"
            | "TasksSetState"
            | "TasksCancel"
        // Workspaces.
            | "WorkspacesCreate"
            | "WorkspacesUpdate"
            | "WorkspacesDelete"
            | "WorkspacesSetSessions"
        // MCP relays: a relay outlives the RPC that created it.
            | "McpCreate"
            | "McpDelete"
            | "McpPublish"
        // Auth, pairing and device lifecycle: a revocation must not race a
        // mutation that the revocation is supposed to fence.
            | "AuthMintBootstrap"
            | "AuthRedeemWorker"
            | "AuthRedeemBrowser"
            | "AuthLogout"
            | "PairCreate"
            | "PairApprove"
            | "PairConfirm"
            | "PairDeny"
            | "DevicesRevoke"
            | "DevicesRotateCurrent"
        // Files, settings and attachments.
            | "FilesMkdir"
            | "TranscriptionSetConfig"
            | "AgentConfigSet"
            | "AttachFileChunk"
            | "DeleteAttachment"
        // Push and UI state.
            | "PushSubscribe"
            | "PushUnsubscribe"
            | "UiApplyLayout"
        // Diagnostics. Only the batch submit is a write; a snapshot is a read
        // and is not in this list.
            | "DiagDebugLogBatch"
    )
}

/// The methods whose **successful** audit row carries no forensic signal.
///
/// Failure rows stay durable for every one of them. The source names the reason
/// for two entries specifically: `WorkersHeartbeat` and `MiscHealth` are volume
/// with no signal, and `PairApprovalStatus` is "the approver's 1 Hz dialog poll"
/// (`apps/coord/src/auth/auth-interceptor.ts:135-144`).
#[must_use]
pub fn method_audit_skips_success(method: &str) -> bool {
    matches!(
        method,
        "AuthCoordIdentity"
            | "DiagDebugLogBatch"
            | "MiscHealth"
            | "WorkersHeartbeat"
            | "PairConfirm"
            | "PairList"
            | "PairApprovalStatus"
            | "SessionsCursorPos"
            | "UiReportState"
            | "SessionsGetScrollbackCells"
            | "SessionsSearchScrollback"
            | "SessionsCancelScrollbackSearch"
            | "SessionsSearchGlobal"
            | "SessionsCancelGlobalSearch"
            | "TranscriptionGetConfig"
    )
}

/// The methods that never write an audit row at all, success or failure.
///
/// One method: `PairPoll`. "Requester polling is token-bound but anonymous. Its
/// valid and invalid outcomes are high-volume, carry no caller identity, and
/// must never create an unsweepable audit row."
/// (`apps/coord/src/auth/auth-interceptor.ts:147-149`.) The same reasoning
/// reaches the rate limiter, which gives this one route 600 tokens a minute so
/// every live requester behind one NAT can still poll -- see §4.11 of
/// `docs/phase3-coord-contract.md`.
#[must_use]
pub fn method_never_persists_audit(method: &str) -> bool {
    method == "PairPoll"
}

/// Whether an audit row is written for this outcome.
///
/// The one special case is a test seam in the source
/// (`apps/coord/src/auth/auth-interceptor.ts:155`): a **failed** `PairConfirm`
/// persists even though a successful one does not, because "who tried to
/// authorize which device, and failed" is the row an operator actually needs.
#[must_use]
pub fn should_persist_method_audit(
    method: &str,
    status: u16,
    pair_confirmation_failed: bool,
) -> bool {
    if method_never_persists_audit(method) {
        return false;
    }
    status != 200
        || !method_audit_skips_success(method)
        || (method == "PairConfirm" && pair_confirmation_failed)
}
