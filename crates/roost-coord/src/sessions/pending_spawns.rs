//! The process-wide dedupe of in-flight session spawns, keyed by the session
//! UUID the spawn will open under. Ports `apps/coord/src/sessions/pending-spawns.ts`.
//!
//! Owned by `SessionsRuntime` (`core.services.sessions`); `sessions::spawn`
//! reserves and settles, `worker_link::frame_dispatch` records a durable
//! `opened`, and the worker lifecycle registry calls it when a credential is revoked.
//!
//! AN AMBIGUOUS FAILURE RETAINS THE RESERVATION. Transport loss, a supersede and
//! the worker-reply deadline all leave the worker's answer unknown, so the
//! reservation waits for the durable `opened` or its own deadline; rejecting
//! early would let one lost `rpc-ok` become a duplicate spawn (an orphan PTY).
//!
//! THE DEADLINES ARE READ WHERE THEY ARE CROSSED. v2 arms two timers per entry;
//! here each entry carries its deadline, every operation sweeps the ones that
//! passed first, and a waiter expires its own entry at the same instant, so what
//! a caller can observe is v2's and no timer task outlives its table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::coord_core::worker_lifecycle::{LinkEnd, WorkerLifecycleObserver};

/// How long a reservation waits for the worker's reply or a durable `opened`.
pub const PENDING_SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a resolved spawn keeps answering exact retries of the same UUID.
pub const COMPLETED_SPAWN_RETENTION: Duration = Duration::from_secs(30);

/// The most reservations, pending or retained, the table holds at once.
pub const MAX_PENDING_SPAWNS: usize = 1_024;

/// Everything a retry must repeat exactly to share a reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSpawnSignature {
    /// The browser fingerprint, tab-scoped when the request named a tab.
    pub caller_key: String,
    /// The worker the spawn is dispatched to.
    pub worker_fp: String,
    /// The requested session kind.
    pub kind: String,
    /// The folder the PTY starts in.
    pub folder: String,
    /// The initial PTY width hint.
    pub cols: Option<u32>,
    /// The initial PTY height hint.
    pub rows: Option<u32>,
}

/// The identity a spawn opened under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSpawnResult {
    /// The session UUID.
    pub session_id: String,
    /// The worker-local keeper channel.
    pub channel_id: u32,
}

type Settlement = Option<Result<PendingSpawnResult, ConnectError>>;

/// What reserving a UUID decided.
#[derive(Debug)]
pub enum SpawnReservation {
    /// This caller owns the dispatch and must send the worker command.
    New(SpawnWaiter),
    /// An exact duplicate is already in flight or retained; share its answer.
    Joined(SpawnWaiter),
    /// The UUID is held by another caller or other parameters.
    Conflict,
    /// The table is full.
    Capacity,
}

/// One caller's view of a reservation's eventual answer.
#[derive(Debug)]
pub struct SpawnWaiter {
    table: Arc<PendingSpawns>,
    session_id: String,
    serial: u64,
    deadline: Instant,
    settled: watch::Receiver<Settlement>,
}

impl SpawnWaiter {
    /// Wait for the reservation to resolve, reject, or cross its deadline.
    pub async fn outcome(mut self) -> Result<PendingSpawnResult, ConnectError> {
        let timed_out =
            tokio::time::timeout_at(self.deadline, self.settled.wait_for(Option::is_some))
                .await
                .is_err();
        if timed_out {
            self.table.expire(&self.session_id, self.serial);
        }
        let settled = self.settled.borrow().clone();
        settled.unwrap_or_else(|| {
            Err(ConnectError::new(
                ErrorCode::Unavailable,
                "the pending spawn was released without settling",
            ))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnPhase {
    Pending {
        ambiguous: bool,
        durable_channel: Option<u32>,
    },
    Resolved,
}

#[derive(Debug)]
struct SpawnEntry {
    serial: u64,
    signature: PendingSpawnSignature,
    phase: SpawnPhase,
    /// The open deadline while pending; the end of retention once resolved.
    deadline: Instant,
    settled: watch::Sender<Settlement>,
}

#[derive(Debug, Default)]
struct SpawnTable {
    entries: HashMap<String, SpawnEntry>,
    next_serial: u64,
}

/// The reservation table.
#[derive(Debug, Default)]
pub struct PendingSpawns {
    table: Mutex<SpawnTable>,
}

impl PendingSpawns {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Atomically reserve a UUID. Exact duplicates share one answer; any
    /// caller or parameter mismatch is refused before a worker command exists.
    pub fn reserve(
        self: &Arc<Self>,
        session_id: &str,
        signature: PendingSpawnSignature,
    ) -> SpawnReservation {
        let mut table = self.lock_swept();
        if let Some(existing) = table.entries.get(session_id) {
            if existing.signature != signature {
                return SpawnReservation::Conflict;
            }
            return SpawnReservation::Joined(self.waiter(session_id, existing));
        }
        if table.entries.len() >= MAX_PENDING_SPAWNS {
            return SpawnReservation::Capacity;
        }
        table.next_serial += 1;
        let (settled, _) = watch::channel(None);
        let entry = SpawnEntry {
            serial: table.next_serial,
            signature,
            phase: SpawnPhase::Pending {
                ambiguous: false,
                durable_channel: None,
            },
            deadline: Instant::now() + PENDING_SPAWN_TIMEOUT,
            settled,
        };
        let waiter = self.waiter(session_id, &entry);
        table.entries.insert(session_id.to_owned(), entry);
        SpawnReservation::New(waiter)
    }

    /// Resolve a pending reservation with its opened identity. False when
    /// nothing is pending under that UUID.
    pub fn resolve(&self, session_id: &str, result: PendingSpawnResult) -> bool {
        let mut table = self.lock_swept();
        let Some(entry) = table.entries.get_mut(session_id) else {
            return false;
        };
        resolve_entry(entry, result)
    }

    /// Record a durable `opened`. The worker's `rpc-ok`, ordered after its first
    /// full frame, remains the normal success; `opened` resolves only a reply
    /// that was lost or otherwise ambiguous.
    pub fn resolve_opened(&self, worker_fp: &str, session_id: &str, channel_id: u32) -> bool {
        let mut table = self.lock_swept();
        let Some(entry) = table.entries.get_mut(session_id) else {
            return false;
        };
        let SpawnPhase::Pending { ambiguous, .. } = entry.phase else {
            return false;
        };
        if entry.signature.worker_fp != worker_fp {
            return false;
        }
        entry.phase = SpawnPhase::Pending {
            ambiguous,
            durable_channel: Some(channel_id),
        };
        resolve_from_durable_opened(session_id, entry);
        true
    }

    /// Settle a pending reservation's failure. An ambiguous one only marks it,
    /// for durable-open reconciliation; a definite one rejects every waiter.
    pub fn reject(&self, session_id: &str, error: ConnectError, definite: bool) -> bool {
        let mut table = self.lock_swept();
        let Some(entry) = table.entries.get_mut(session_id) else {
            return false;
        };
        let SpawnPhase::Pending {
            durable_channel, ..
        } = entry.phase
        else {
            return false;
        };
        if !definite {
            entry.phase = SpawnPhase::Pending {
                ambiguous: true,
                durable_channel,
            };
            resolve_from_durable_opened(session_id, entry);
            return true;
        }
        if let Some(entry) = table.entries.remove(session_id) {
            tracing::info!(session_id, error = %error, "sessions: a pending spawn was rejected");
            entry.settled.send_replace(Some(Err(error)));
        }
        true
    }

    /// Drop every reservation for a worker whose credential was revoked,
    /// rejecting the pending ones: no replay through that credential generation
    /// can reconcile them. Returns how many pending spawns were rejected.
    pub fn reject_for_worker(&self, worker_fp: &str) -> usize {
        let mut table = self.lock_swept();
        let doomed: Vec<String> = table
            .entries
            .iter()
            .filter(|(_, entry)| entry.signature.worker_fp == worker_fp)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        let mut rejected = 0;
        for session_id in doomed {
            let Some(entry) = table.entries.remove(&session_id) else {
                continue;
            };
            if matches!(entry.phase, SpawnPhase::Pending { .. }) {
                rejected += 1;
                entry.settled.send_replace(Some(Err(ConnectError::new(
                    ErrorCode::Unauthenticated,
                    "worker credential revoked",
                ))));
            }
        }
        rejected
    }

    /// Time a reservation out on behalf of a waiter that reached its deadline,
    /// if that exact entry is still pending.
    fn expire(&self, session_id: &str, serial: u64) {
        let mut table = self.lock();
        let still_pending = table.entries.get(session_id).is_some_and(|entry| {
            entry.serial == serial && matches!(entry.phase, SpawnPhase::Pending { .. })
        });
        if still_pending && let Some(entry) = table.entries.remove(session_id) {
            time_out(session_id, &entry);
        }
    }

    fn waiter(self: &Arc<Self>, session_id: &str, entry: &SpawnEntry) -> SpawnWaiter {
        SpawnWaiter {
            table: Arc::clone(self),
            session_id: session_id.to_owned(),
            serial: entry.serial,
            deadline: entry.deadline,
            settled: entry.settled.subscribe(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SpawnTable> {
        // Every critical section is a map operation; a poisoned lock can only
        // be an allocation failure, and one must not wedge every later spawn.
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The table, with every entry whose deadline passed already retired.
    fn lock_swept(&self) -> MutexGuard<'_, SpawnTable> {
        let mut table = self.lock();
        let now = Instant::now();
        let expired: Vec<String> = table
            .entries
            .iter()
            .filter(|(_, entry)| entry.deadline <= now)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        for session_id in expired {
            if let Some(entry) = table.entries.remove(&session_id)
                && matches!(entry.phase, SpawnPhase::Pending { .. })
            {
                time_out(&session_id, &entry);
            }
        }
        table
    }
}

/// A revoked credential makes every unresolved spawn on that worker impossible
/// (v2 `worker-conn.ts` `revoke()`). A close or a supersede is NOT a rejection:
/// the pending-RPC table fails the worker reply as `Unavailable`, which the
/// spawn treats as ambiguous and reconciles against the durable `opened`.
impl WorkerLifecycleObserver for PendingSpawns {
    fn on_closed(&self, handle: &Arc<WorkerHandle>, end: LinkEnd) {
        if end != LinkEnd::Revoked {
            return;
        }
        let rejected = self.reject_for_worker(handle.worker_fp.as_str());
        if rejected > 0 {
            tracing::info!(
                worker_fp = %handle.worker_fp,
                rejected,
                "sessions: pending spawns were rejected on credential revocation"
            );
        }
    }
}

fn resolve_entry(entry: &mut SpawnEntry, result: PendingSpawnResult) -> bool {
    if !matches!(entry.phase, SpawnPhase::Pending { .. }) {
        return false;
    }
    entry.phase = SpawnPhase::Resolved;
    entry.deadline = Instant::now() + COMPLETED_SPAWN_RETENTION;
    tracing::info!(
        session_id = result.session_id,
        channel_id = result.channel_id,
        "sessions: a pending spawn resolved"
    );
    entry.settled.send_replace(Some(Ok(result)));
    true
}

fn resolve_from_durable_opened(session_id: &str, entry: &mut SpawnEntry) {
    if let SpawnPhase::Pending {
        ambiguous: true,
        durable_channel: Some(channel_id),
    } = entry.phase
    {
        resolve_entry(
            entry,
            PendingSpawnResult {
                session_id: session_id.to_owned(),
                channel_id,
            },
        );
    }
}

fn time_out(session_id: &str, entry: &SpawnEntry) {
    let timeout_ms = PENDING_SPAWN_TIMEOUT.as_millis();
    tracing::warn!(
        session_id,
        timeout_ms,
        "sessions: a pending spawn never durably opened"
    );
    entry.settled.send_replace(Some(Err(ConnectError::new(
        ErrorCode::DeadlineExceeded,
        format!("spawn {session_id} did not durably open within {timeout_ms}ms"),
    ))));
}
