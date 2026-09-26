//! The coordinator-side correlation table for browser-to-worker RPCs that need
//! a reply: a scrollback page, a search, a spawn.
//!
//! Ported from `apps/coord/src/router/pending-rpcs.ts`. v2 keeps it in a
//! module-level `Map`; here it is a value the boot path constructs and hands
//! down, because `services.rs` states that nothing in this crate reaches for a
//! global and because a crate-root `static` would make every test in the
//! coordinator share one table -- a test that published would perturb the next.
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
use std::sync::{Arc, Mutex};

use connectrpc::{ConnectError, ErrorCode};
use tokio::sync::oneshot;

/// The default a caller that names no deadline gets.
pub const DEFAULT_PENDING_RPC_TIMEOUT_MS: u64 = 30_000;

/// How one outstanding browser-to-worker RPC finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOutcome {
    /// The worker answered.
    Resolved(serde_json::Value),
    /// A permanent command failure the worker reported.
    Rejected(String),
    /// The browser abandoned it.
    Cancelled,
    /// It never reached the worker. Retryable, which is the difference from
    /// [`Self::Rejected`].
    Unavailable(String),
}

impl PendingOutcome {
    /// The reply, or the error this outcome becomes on the wire.
    pub fn into_result(self) -> Result<serde_json::Value, ConnectError> {
        match self {
            Self::Resolved(payload) => Ok(payload),
            Self::Rejected(message) => Err(ConnectError::new(
                ErrorCode::Internal,
                if message.is_empty() {
                    "worker rpc failed".to_owned()
                } else {
                    message
                },
            )),
            Self::Cancelled => Err(ConnectError::new(
                ErrorCode::Canceled,
                "browser request cancelled",
            )),
            Self::Unavailable(message) => Err(ConnectError::new(
                ErrorCode::Unavailable,
                if message.is_empty() {
                    "worker transport unavailable".to_owned()
                } else {
                    message
                },
            )),
        }
    }
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

    /// Wait for the worker's reply, or for whichever side abandons it.
    pub async fn settle(&mut self) -> Result<serde_json::Value, ConnectError> {
        match (&mut self.receiver).await {
            Ok(outcome) => outcome.into_result(),
            Err(_) => Err(ConnectError::new(
                ErrorCode::Unavailable,
                "the pending worker RPC was released without settling",
            )),
        }
    }
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
}

impl PendingRpcs {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
    if let Ok(mut sender) = entry.sender.lock() {
        if let Some(open) = sender.take() {
            let _ = open.send(outcome);
        }
    }
}
