//! The shared state a scrollback read needs: the worker registry, the pending
//! correlation table, and the cancel tombstone ledger.
//!
//! Owned by the coordinator's terminal-screen domain. One value, constructed at
//! boot beside the other process singletons, because all three of these are
//! process state that v2 kept in module-level `Map`s and `services.rs`'s own
//! header forbids: *"nothing in this crate reaches for a global."*
//!
//! The relay also owns the ONE thing a scrollback read is really made of: the
//! join from a session to the worker that runs it, and the refusal wording when
//! that join finds nothing. v2 spread that preamble across every forwarding
//! handler with drifted wordings ("unknown session" vs "session not found");
//! two wordings are not two facts, so they live here once.

use std::sync::{Arc, Mutex};

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::wire::{SessionId, WorkerFp};

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::db::CoordDb;
use crate::terminal_screen::pending_rpcs::PendingRpcs;
use crate::terminal_screen::search_ledger::{ScrollbackSearchLedger, SearchIdentity};

/// A session's live worker socket.
#[derive(Debug, Clone)]
pub struct SessionWorkerBinding {
    /// The worker the session's row names.
    pub worker_fp: WorkerFp,
    /// That worker's current routable socket generation.
    pub handle: Arc<WorkerHandle>,
}

/// Everything a scrollback RPC needs, in one injectable value.
#[derive(Clone)]
pub struct ScrollbackRelay {
    workers: Arc<WorkerRegistry>,
    pending: Arc<PendingRpcs>,
    ledger: Arc<Mutex<ScrollbackSearchLedger>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for ScrollbackRelay {
    /// The relay holds a clock, and a log line needs the pending depth rather
    /// than the clock's value.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScrollbackRelay")
            .field("pending", &self.pending.pending_count())
            .finish_non_exhaustive()
    }
}

impl ScrollbackRelay {
    /// A relay over the process registry, reading the real clock.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>) -> Self {
        Self {
            workers,
            pending: Arc::new(PendingRpcs::new()),
            ledger: Arc::new(Mutex::new(ScrollbackSearchLedger::new())),
            now_ms: Arc::new(crate::serve::now_ms),
        }
    }

    /// A relay whose clock the caller supplies, so a test never waits for this
    /// one to tick.
    #[must_use]
    pub fn with_clock(
        workers: Arc<WorkerRegistry>,
        now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            now_ms,
            ..Self::new(workers)
        }
    }

    /// The process registry this relay routes through.
    #[must_use]
    pub fn workers(&self) -> &Arc<WorkerRegistry> {
        &self.workers
    }

    /// The browser-to-worker correlation table.
    #[must_use]
    pub fn pending(&self) -> &Arc<PendingRpcs> {
        &self.pending
    }

    /// The relay's clock.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        (self.now_ms)()
    }

    /// The tab-scoped identity a search and its cancel share.
    ///
    /// A search with no tab id is refused rather than filed under the bare
    /// fingerprint: two tabs of one device would then share a cancellation
    /// namespace, and cancelling in one would silently kill the other.
    pub fn viewer_id(
        caller_fingerprint: &str,
        tab_id: Option<&str>,
    ) -> Result<String, ConnectError> {
        match tab_id {
            Some(tab_id) if !tab_id.is_empty() => Ok(format!("{caller_fingerprint}:{tab_id}")),
            _ => Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "a scrollback search requires a browser tab id (x-roost-tab-id)",
            )),
        }
    }

    /// The identity a cancel is filed under. A cancel without a tab falls back
    /// to the bare fingerprint, because refusing it would leave a search the
    /// caller can no longer reach.
    #[must_use]
    pub fn cancel_viewer_id(caller_fingerprint: &str, tab_id: Option<&str>) -> String {
        match tab_id {
            Some(tab_id) if !tab_id.is_empty() => format!("{caller_fingerprint}:{tab_id}"),
            _ => caller_fingerprint.to_owned(),
        }
    }

    /// The identity of one search.
    #[must_use]
    pub fn search_identity(
        &self,
        viewer_id: &str,
        session_id: &SessionId,
        search_id: &str,
    ) -> SearchIdentity {
        SearchIdentity::new(viewer_id, session_id.as_str(), search_id)
    }

    /// Record a cancel so a search that has not been forwarded yet is still
    /// retired when it arrives.
    pub fn record_cancel(&self, identity: &SearchIdentity) {
        let now_ms = self.now_ms();
        if let Ok(mut ledger) = self.ledger.lock() {
            ledger.record_cancel(identity, now_ms);
        }
    }

    /// Retire a tombstone, if one is live. True means this search was already
    /// cancelled and must not be forwarded.
    pub fn consume_cancel(&self, identity: &SearchIdentity) -> bool {
        let now_ms = self.now_ms();
        self.ledger
            .lock()
            .map(|mut ledger| ledger.consume_cancel(identity, now_ms))
            .unwrap_or(false)
    }

    /// How many cancel tombstones are live, for a diagnostics answer.
    #[must_use]
    pub fn live_tombstones(&self) -> usize {
        let now_ms = self.now_ms();
        self.ledger
            .lock()
            .map(|ledger| ledger.live_count(now_ms))
            .unwrap_or_default()
    }

    /// Load a session's live worker socket, or say which of the two facts is
    /// missing.
    ///
    /// A tombstoned worker row and a session that no longer exists are
    /// different answers: the first is a live session whose machine is gone,
    /// the second is a caller naming something that never was. Both are
    /// `NotFound` on the wire, because the browser acts the same on both -- but
    /// the log line, and therefore the operator, must not.
    pub async fn session_worker_socket(
        &self,
        db: &CoordDb,
        session_id: &SessionId,
    ) -> Result<SessionWorkerBinding, ConnectError> {
        let worker_fp: Option<String> = sqlx::query_scalar(
            "SELECT session.worker_fp FROM sessions AS session
             INNER JOIN workers AS worker ON worker.fp = session.worker_fp
             WHERE session.id = ?1 AND worker.deleted_at_ms IS NULL",
        )
        .bind(session_id.as_str())
        .fetch_optional(db.pool())
        .await
        .map_err(|error| {
            tracing::error!(session_id = %session_id, %error, "the session worker lookup failed");
            ConnectError::new(ErrorCode::Internal, "session not found")
        })?
        .flatten();
        let Some(worker_fp) = worker_fp else {
            return Err(ConnectError::new(ErrorCode::NotFound, "session not found"));
        };
        let fingerprint = WorkerFp::try_from(worker_fp).map_err(|error| {
            tracing::error!(session_id = %session_id, %error, "a sessions row names a malformed worker fingerprint");
            ConnectError::new(ErrorCode::Internal, "session not found")
        })?;
        let handle = self
            .workers
            .current_routable(&fingerprint)
            .ok_or_else(|| ConnectError::new(ErrorCode::Unavailable, "worker offline"))?;
        Ok(SessionWorkerBinding {
            worker_fp: fingerprint,
            handle,
        })
    }
}
