//! The coordinator-side correlation table for coordinator-to-worker requests
//! that need a reply: a scrollback page, a search, a spawn, a terminal input.
//!
//! Ported from `apps/coord/src/router/pending-rpcs.ts`. v2 keeps it in a
//! module-level `Map`; here it is a value the boot path constructs and hands
//! down (`services.scrollback.pending()`), because nothing in this crate reaches
//! for a global -- and that includes the request-id sequence, which is this
//! table's own field rather than a crate-root `static` every test would share.
//!
//! The map key is `(worker fingerprint, request id)`, never the request id
//! alone: a client-supplied correlation id must not be able to settle another
//! worker's request, and one worker's ids must not collide with another's.
//!
//! WHERE THE DEADLINE LIVES. v2 arms a timer inside the table entry. Here the
//! entry holds a `oneshot` and the CALLER owns the deadline, because the caller
//! is the only one that knows which error a timeout should become: a search
//! deadline is `Unavailable`, a page deadline is `Unavailable` with different
//! wording, and a table that answered for both would have to guess.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use connectrpc::{ConnectError, ErrorCode};
use tokio::sync::oneshot;

use crate::terminal_screen::typed_results::{TypedResult, TypedWorkerResult};

/// The default a caller that names no deadline gets.
pub const DEFAULT_PENDING_RPC_TIMEOUT_MS: u64 = 30_000;

/// How one outstanding coordinator-to-worker RPC finished.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingOutcome {
    /// The worker answered with an `rpc-ok` body.
    Resolved(serde_json::Value),
    /// The worker answered with a typed result frame.
    Answered(TypedWorkerResult),
    /// A permanent command failure the worker reported.
    Rejected(String),
    /// The browser abandoned it.
    Cancelled,
    /// It never reached the worker. Retryable, which is the difference from
    /// [`Self::Rejected`].
    Unavailable(String),
}

impl PendingOutcome {
    /// The `rpc-ok` body, or the error this outcome becomes on the wire.
    pub fn into_result(self) -> Result<serde_json::Value, ConnectError> {
        match self {
            Self::Resolved(payload) => Ok(payload),
            Self::Answered(result) => Err(wrong_kind(result.kind(), "rpc-ok")),
            Self::Rejected(message) => Err(rejected(message)),
            Self::Cancelled => Err(cancelled()),
            Self::Unavailable(message) => Err(unavailable(message)),
        }
    }

    /// The typed result the waiter named, or the error this outcome becomes.
    ///
    /// A result of another kind under the same id is the worker answering a
    /// different question; it is refused rather than read as this one.
    pub fn into_typed<T: TypedResult>(self) -> Result<T, ConnectError> {
        match self {
            Self::Answered(result) => {
                T::from_typed(result).map_err(|answered| wrong_kind(answered, T::KIND))
            }
            Self::Resolved(_) => Err(wrong_kind("rpc-ok", T::KIND)),
            Self::Rejected(message) => Err(rejected(message)),
            Self::Cancelled => Err(cancelled()),
            Self::Unavailable(message) => Err(unavailable(message)),
        }
    }
}

fn rejected(message: String) -> ConnectError {
    let message = if message.is_empty() {
        "worker rpc failed".to_owned()
    } else {
        message
    };
    ConnectError::new(ErrorCode::Internal, message)
}

fn cancelled() -> ConnectError {
    ConnectError::new(ErrorCode::Canceled, "browser request cancelled")
}

fn unavailable(message: String) -> ConnectError {
    let message = if message.is_empty() {
        "worker transport unavailable".to_owned()
    } else {
        message
    };
    ConnectError::new(ErrorCode::Unavailable, message)
}

fn wrong_kind(answered: &str, expected: &str) -> ConnectError {
    ConnectError::new(
        ErrorCode::Internal,
        format!("the worker answered with {answered} where {expected} was expected"),
    )
}

type Sender = Option<oneshot::Sender<PendingOutcome>>;

#[derive(Debug)]
struct PendingEntry {
    worker_fp: Option<String>,
    created_at_ms: i64,
    sender: Mutex<Sender>,
}

/// The waiting half of one pending RPC.
#[derive(Debug)]
pub struct PendingRpc {
    request_id: String,
    worker_fp: Option<String>,
    table: Arc<PendingRpcs>,
    receiver: oneshot::Receiver<PendingOutcome>,
}

impl PendingRpc {
    /// The correlation id to put on the downstream envelope.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The worker this RPC is namespaced to, if it was namespaced.
    #[must_use]
    pub fn worker_fp(&self) -> Option<&str> {
        self.worker_fp.as_deref()
    }

    /// When the entry was opened, for a diagnostics answer.
    #[must_use]
    pub fn created_at_ms(&self) -> i64 {
        self.table
            .created_at_ms(&self.request_id, self.worker_fp.as_deref())
            .unwrap_or_default()
    }

    /// Wait for the worker's `rpc-ok` body, or for whichever side abandons it.
    pub async fn settle(&mut self) -> Result<serde_json::Value, ConnectError> {
        match (&mut self.receiver).await {
            Ok(outcome) => outcome.into_result(),
            Err(_) => Err(released()),
        }
    }

    /// Wait for the worker's typed result, or for whichever side abandons it.
    pub async fn settle_typed<T: TypedResult>(&mut self) -> Result<T, ConnectError> {
        match (&mut self.receiver).await {
            Ok(outcome) => outcome.into_typed(),
            Err(_) => Err(released()),
        }
    }
}

fn released() -> ConnectError {
    ConnectError::new(
        ErrorCode::Unavailable,
        "the pending worker RPC was released without settling",
    )
}

impl Drop for PendingRpc {
    /// Releasing the half the coordinator holds IS the cancellation: the entry
    /// leaves the table so a late worker reply cannot settle a caller that is
    /// gone, and no timer has to be torn down by hand.
    fn drop(&mut self) {
        if let Ok(mut entries) = self.table.entries.lock() {
            entries.remove(&PendingRpcs::key(
                &self.request_id,
                self.worker_fp.as_deref(),
            ));
        }
    }
}

/// The correlation table.
#[derive(Debug, Default)]
pub struct PendingRpcs {
    entries: Mutex<HashMap<(String, String), Arc<PendingEntry>>>,
    sequence: AtomicU64,
}

impl PendingRpcs {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh correlation id for one coordinator-to-worker request.
    ///
    /// v2 mints a v4 uuid per request. This is a monotonic counter: the id only
    /// has to be unique among this table's entries, and the table namespaces it
    /// by worker anyway, so a random source would buy a property nothing checks.
    #[must_use]
    pub fn next_request_id(&self) -> String {
        format!("rpc-{}", self.sequence.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// Open one entry under a freshly minted id (v2 `createPendingRpc`;
    /// [`Self::create`] is `createPendingRpcWithId`, for a caller-supplied id).
    pub fn create_fresh(
        self: &Arc<Self>,
        worker_fp: Option<&str>,
        now_ms: i64,
    ) -> Result<PendingRpc, ConnectError> {
        self.create(&self.next_request_id(), worker_fp, now_ms)
    }

    /// Open one entry, refusing a duplicate rather than overwriting it: the
    /// first caller owns the completion, and silently replacing it would leave
    /// that caller's reply with nowhere to go.
    pub fn create(
        self: &Arc<Self>,
        request_id: &str,
        worker_fp: Option<&str>,
        now_ms: i64,
    ) -> Result<PendingRpc, ConnectError> {
        let key = Self::key(request_id, worker_fp);
        let (sender, receiver) = oneshot::channel();
        let entry = Arc::new(PendingEntry {
            worker_fp: worker_fp.map(str::to_owned),
            created_at_ms: now_ms,
            sender: Mutex::new(Some(sender)),
        });
        {
            let mut entries = self.entries.lock().map_err(|_| {
                ConnectError::new(ErrorCode::Internal, "pending RPC table is poisoned")
            })?;
            if entries.contains_key(&key) {
                return Err(ConnectError::new(
                    ErrorCode::AlreadyExists,
                    "request_id is already pending for this worker",
                ));
            }
            entries.insert(key, entry);
        }
        Ok(PendingRpc {
            request_id: request_id.to_owned(),
            worker_fp: worker_fp.map(str::to_owned),
            table: Arc::clone(self),
            receiver,
        })
    }

    /// Settle a request with the worker's reply. False means nothing was
    /// waiting under that identity: a stale, duplicated or late `rpc-ok`.
    pub fn resolve(
        &self,
        request_id: &str,
        payload: serde_json::Value,
        worker_fp: Option<&str>,
    ) -> bool {
        self.settle(request_id, worker_fp, PendingOutcome::Resolved(payload))
    }

    /// Settle a request with a typed result frame, which carries its own id.
    /// False, as for [`Self::resolve`], when nothing waits under that identity.
    pub fn resolve_typed(&self, result: TypedWorkerResult, worker_fp: Option<&str>) -> bool {
        let Some(entry) = self.take(result.request_id(), worker_fp) else {
            return false;
        };
        deliver(&entry, PendingOutcome::Answered(result));
        true
    }

    /// Settle a request with a permanent worker command failure.
    pub fn reject(&self, request_id: &str, message: &str, worker_fp: Option<&str>) -> bool {
        self.settle(
            request_id,
            worker_fp,
            PendingOutcome::Rejected(if message.is_empty() {
                "worker rpc failed".to_owned()
            } else {
                message.to_owned()
            }),
        )
    }

    /// Settle a request whose transport never carried it.
    pub fn reject_unavailable(
        &self,
        request_id: &str,
        message: &str,
        worker_fp: Option<&str>,
    ) -> bool {
        self.settle(
            request_id,
            worker_fp,
            PendingOutcome::Unavailable(if message.is_empty() {
                "worker transport unavailable".to_owned()
            } else {
                message.to_owned()
            }),
        )
    }

    /// Settle a request the browser abandoned.
    pub fn cancel(&self, request_id: &str, worker_fp: Option<&str>) -> bool {
        self.settle(request_id, worker_fp, PendingOutcome::Cancelled)
    }

    /// Reject every in-flight RPC routed to one worker, and say how many.
    ///
    /// Called when a worker's socket closes: a browser fast-fails with a
    /// retryable error instead of hanging until the deadline.
    pub fn reject_all_for_worker(&self, worker_fp: &str, message: &str) -> usize {
        let Ok(mut entries) = self.entries.lock() else {
            return 0;
        };
        let doomed: Vec<(String, String)> = entries
            .iter()
            .filter(|(_, entry)| entry.worker_fp.as_deref() == Some(worker_fp))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &doomed {
            if let Some(entry) = entries.remove(key) {
                deliver(
                    &entry,
                    PendingOutcome::Unavailable(format!(
                        "{message} (worker {worker_fp} disconnected)"
                    )),
                );
            }
        }
        if !doomed.is_empty() {
            tracing::warn!(
                worker_fp,
                count = doomed.len(),
                "in-flight worker RPCs were rejected on worker close"
            );
        }
        doomed.len()
    }

    /// How many entries are open, for a diagnostics answer.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or_default()
    }

    fn created_at_ms(&self, request_id: &str, worker_fp: Option<&str>) -> Option<i64> {
        let entries = self.entries.lock().ok()?;
        entries
            .get(&Self::key(request_id, worker_fp))
            .map(|entry| entry.created_at_ms)
    }

    fn settle(&self, request_id: &str, worker_fp: Option<&str>, outcome: PendingOutcome) -> bool {
        let Some(entry) = self.take(request_id, worker_fp) else {
            return false;
        };
        deliver(&entry, outcome);
        true
    }

    fn take(&self, request_id: &str, worker_fp: Option<&str>) -> Option<Arc<PendingEntry>> {
        let mut entries = self.entries.lock().ok()?;
        let key = Self::key(request_id, worker_fp);
        if let Some(entry) = entries.remove(&key) {
            return Some(entry);
        }
        if worker_fp.is_some() {
            let untagged = Self::key(request_id, None);
            if let Some(entry) = entries.remove(&untagged) {
                return Some(entry);
            }
        }
        None
    }

    fn key(request_id: &str, worker_fp: Option<&str>) -> (String, String) {
        (
            worker_fp.unwrap_or_default().to_owned(),
            request_id.to_owned(),
        )
    }
}

fn deliver(entry: &Arc<PendingEntry>, outcome: PendingOutcome) {
    if let Ok(mut sender) = entry.sender.lock()
        && let Some(open) = sender.take()
    {
        let _ = open.send(outcome);
    }
}
