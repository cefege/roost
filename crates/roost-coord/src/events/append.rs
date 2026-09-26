//! The one durable write in the coordinator: admit, insert, project, and then --
//! strictly after the commit -- publish.
//!
//! Ported from `apps/coord/src/events/event-transaction.ts`. This file is the
//! orchestration and the shapes; `append_transaction` is the transaction body and
//! `append_publication` is the strictly-after-commit half. The three-way split is
//! the contract: `docs/phase3-coord-contract.md` §3.1 pins the order inside the
//! transaction and §3.2 pins why "after" is structural rather than conventional.
//!
//! **BEFORE THE TRANSACTION.** Re-validate an `agent_reference` against the event
//! schema; refuse a snapshot over 1,024 sessions; **normalize** the six
//! worker-controlled strings; and reserve the publication slot. Normalization is
//! before any durable write so replay and projection stay byte-identical, and the
//! reservation is before the commit so two concurrent deliveries of one
//! `client_seq` serialize instead of double-publishing.
//!
//! **INSIDE ONE TRANSACTION.** Admission first -- a refusal is a data outcome
//! that writes nothing, which is why a foreign worker's `extra_work` can never
//! run. Then a `workspace_assigned` naming a missing workspace **throws** and rolls
//! back, deliberately harsher than admission because retrying cannot help. Then
//! the snapshot tombstone filter, applied to the *effective* event before it is
//! serialized so the log, the projection, the route index and the Sync
//! publication all see the same set. Then the caller's atomic extra work. Then the
//! insert, with `ON CONFLICT (worker_fp, client_seq) DO NOTHING` against the
//! partial unique index. Then the dedupe short-circuit: no inserted id plus a
//! non-null `client_seq` means already applied, so the projection is skipped, the
//! publication is skipped, and the caller still gets its ACK.
//!
//! **AFTER COMMIT.** Never before, and not from inside the transaction body.
//!
//! WHERE THE THREE FILES MEET. `append_input` is everything before and after the
//! commit that is not in it; `append_transaction` is the body, and it cannot name
//! the publisher; `append_publication` holds the publisher and is reachable only
//! from here. §3.2's "the transaction body contains no publish call at all" is
//! therefore a property of the module graph, not of a reviewer's reading.
//!
//! WHY THE FINGERPRINT IN `Caller` IS THE AUTHENTICATED ONE. An event's own
//! `worker_fp` is the worker's claim about itself and is only ever checked for
//! agreement; `Caller::worker_fp` is what the transport authenticated. A
//! `respawned` from a producer with no authenticated fingerprint binds no channel
//! at all, because inferring the worker from the route cache could bind on a
//! worker that has already been replaced
//! (`apps/coord/tests/durable-publication.test.ts:98-107`).

use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;
use roost_protocol::wire::{SessionEvent, WorkerFp};
use sqlx::sqlite::SqliteConnection;

use crate::db::CoordDb;
use crate::events::agent_conversation_recovery::AgentConversationRecoveryError;
use crate::events::append_publication::{LivePublication, PublicationResolution, resolve_publication};
use crate::events::append_input::{prepare_event, release_reservation, reserve_publication_slot};
use crate::events::append_transaction::{CommittedState, run_in_transaction};
use crate::events::bus_domains::Buses;
use crate::events::pending_publications::PendingPublicationStore;
use crate::events::projection::ProjectionError;

/// Who is appending, and the dashboard every write is scoped to.
///
/// A worker caller carries a fingerprint and its outbox sequence; a coordinator
/// caller -- the synthetic ghost close, the deploy-line path -- carries neither,
/// and the partial unique index ignores rows with a null on either side
/// (`event-transaction.ts:29-38`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated worker fingerprint, never a worker-frame claim. `None`
    /// for a coordinator-side producer, which admission always admits.
    pub worker_fp: Option<WorkerFp>,
    /// The worker's monotonic outbox sequence. `None` for a coordinator-side
    /// producer, and the reason such an event can never be deduplicated.
    pub client_seq: Option<u64>,
    /// The value stamped into every `dashboard_id` column this append writes.
    pub dashboard_id: String,
}

impl Caller {
    /// A worker caller, by its authenticated fingerprint.
    #[must_use]
    pub fn worker(worker_fp: WorkerFp, client_seq: u64, dashboard_id: &str) -> Self {
        Self {
            worker_fp: Some(worker_fp),
            client_seq: Some(client_seq),
            dashboard_id: dashboard_id.to_owned(),
        }
    }

    /// A coordinator-side producer: no fingerprint, no sequence, no dedupe.
    #[must_use]
    pub fn coordinator(dashboard_id: &str) -> Self {
        Self {
            worker_fp: None,
            client_seq: None,
            dashboard_id: dashboard_id.to_owned(),
        }
    }

    /// The publication slot this caller's event belongs in, when it has one.
    #[must_use]
    pub fn reservation(&self) -> Option<Reservation> {
        match (&self.worker_fp, self.client_seq) {
            (Some(worker_fp), Some(client_seq)) => Some(Reservation {
                worker_fp: worker_fp.clone(),
                client_seq,
            }),
            _ => None,
        }
    }
}

/// A reserved publication slot's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    /// The worker whose outbox the sequence belongs to.
    pub worker_fp: WorkerFp,
    /// The worker's monotonic outbox sequence.
    pub client_seq: u64,
}

/// What an append decided, and what the caller must now do about it.
#[derive(Debug, Clone, PartialEq)]
pub struct AppendEventResult {
    /// False when persisted worker or resource ownership could not be proven.
    /// A refusal writes nothing, and gets **no ACK and no close**: a prober cannot
    /// be allowed to tell "never existed" from "not yours".
    pub admitted: bool,
    /// True only when this call inserted the durable event row.
    pub inserted: bool,
    /// True only after the committed event updated the volatile routes and Sync.
    pub published: bool,
    /// A dedupe replay carried a different payload for the same `client_seq`. A
    /// protocol violation for the transport to close on.
    pub replay_rejected: bool,
    /// The normalized, tombstone-filtered event this append selected.
    pub event: SessionEvent,
    /// Force-closed snapshot members omitted from `event` and awaiting a reap.
    pub snapshot_reap_ids: Vec<String>,
}

/// Why an append failed, as distinct from a refusal.
#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    /// A statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// A projection read or write failed.
    #[error("projection: {0}")]
    Projection(#[from] ProjectionError),
    /// A recovery reference was refused.
    #[error("agent conversation recovery: {0}")]
    AgentRecovery(#[from] AgentConversationRecoveryError),
    /// The event could not be encoded or re-decoded.
    #[error("event encoding: {0}")]
    Encode(#[from] serde_json::Error),
    /// An `agent_reference` broke the event schema at the transaction boundary.
    #[error("invalid agent conversation reference event")]
    InvalidAgentReference,
    /// A snapshot announced more sessions than the cap allows.
    #[error("worker snapshot exceeds {limit} sessions, got {sessions}")]
    SnapshotTooLarge {
        /// How many the snapshot announced.
        sessions: usize,
        /// The cap.
        limit: usize,
    },
    /// A `workspace_assigned` named a workspace that does not exist. Thrown rather
    /// than refused, because the workspace is missing and a retry cannot help.
    #[error("workspace is unavailable")]
    WorkspaceUnavailable,
    /// An `agent_reference` arrived from a producer with no worker identity, so the
    /// monotonic guard it needs cannot be written.
    #[error("agent conversation reference requires worker delivery")]
    AgentReferenceNeedsWorkerDelivery,
    /// The bounded publication store is full. Refused before the transaction
    /// opened, so a full store costs the caller nothing but the retry.
    #[error("pending event publication capacity exceeded")]
    PublicationCapacity,
    /// A durable id is not a value the wire can carry.
    #[error("durable event id {id} is out of range")]
    EventIdOutOfRange {
        /// The offending value.
        id: i64,
    },
    /// A retained effect's stored JSON no longer decodes. A data fault, reported
    /// rather than swallowed, because publishing it would publish nothing.
    #[error("retained event {event_id} could not be decoded: {reason}")]
    UndecodableRetained {
        /// The event whose stored payload is unreadable.
        event_id: i64,
        /// What the decoder said.
        reason: String,
    },
    /// The caller's atomic extra work failed, and the whole append rolled back with
    /// it.
    #[error("atomic extra work: {0}")]
    ExtraWork(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A worker sequence is past the range the durable column can hold.
    #[error("client_seq {client_seq} exceeds the durable sequence range")]
    ClientSeqOutOfRange {
        /// The offending sequence.
        client_seq: u64,
    },
}

/// The live, in-process effects a committed event has, owned by the domains that
/// implement them.
/// Two methods, both in the post-commit half, and neither may be a no-op: the
/// first is the durable channel index the terminal hub applies (a cell frame that
/// routes before the event that named its channel is a frame nobody can place), and
/// the second is the browser-command kill a snapshot reap sends (a force-closed
/// PTY that is never killed is a session row that outlives its process). There are
/// no default bodies, because a default that silently does nothing is exactly the
/// history-corrupting drop this subsystem exists to prevent.
pub trait LiveEffects: Send + Sync {
    /// Apply the durable channel index for a committed event.
    ///
    /// `authenticated_worker_fp` is `None` for a coordinator-side producer, and
    /// `None` must bind nothing: inferring the worker from the route cache could
    /// bind on a worker that has already been replaced.
    fn index_durable_channel(&self, event: &SessionEvent, authenticated_worker_fp: Option<&WorkerFp>);

    /// Kill a PTY the coordinator force-closed while its worker was offline.
    fn kill_orphan_pty(&self, worker_fp: &WorkerFp, session_id: &str);
}

/// A write that must commit atomically with the event.
///
/// It runs inside the transaction and **after** ownership admission, so a foreign
/// request cannot make an auxiliary mutation -- that ordering is the whole reason
/// this is a callback and not a second transaction (`event-transaction.ts:71-74`).
pub type AtomicExtraWork<'a> = Box<
    dyn for<'connection> FnMut(
            &'connection mut SqliteConnection,
        ) -> BoxFuture<'connection, Result<(), Box<dyn std::error::Error + Send + Sync>>>
        + Send
        + 'a,
>;

/// Everything one append needs beyond the event and the caller.
pub struct AppendOptions<'a> {
    /// The caller's wall clock, in epoch milliseconds. It is a parameter so the core
    /// reads no clock of its own and a test can pin the junction timestamp; the
    /// append path needs exactly one.
    pub now_ms: i64,
    /// The buses the publication half publishes to.
    pub buses: &'a Buses,
    /// The post-commit live effects.
    pub live_effects: &'a dyn LiveEffects,
    /// The bounded publication store, when the process has one.
    ///
    /// A shared handle rather than a borrow, and a `std::sync::Mutex` rather than
    /// a `tokio` one: every critical section in the append path -- reserve, claim,
    /// retain, release -- is synchronous, so none of them needs to be held across
    /// an await and none of them serializes the transaction behind it. A caller
    /// that reaches the store directly (revocation, worker delete) uses the same
    /// handle.
    pub pending_publications: Option<Arc<Mutex<PendingPublicationStore>>>,
    /// The generation and revocation fence, consulted **after** the commit. False
    /// keeps the durable row and its projection and suppresses every live effect.
    pub can_publish: Option<&'a dyn Fn() -> bool>,
    /// Writes that must commit with the event.
    pub extra_work: Option<AtomicExtraWork<'a>>,
    /// Worker connections defer orphan reaps until their snapshot ACK barrier has
    /// made the exact current handle ready. Direct coordinator callers do not.
    pub defer_snapshot_reap: bool,
}

impl std::fmt::Debug for AppendOptions<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppendOptions")
            .field("now_ms", &self.now_ms)
            .field("fenced", &self.can_publish.is_some())
            .field("has_extra_work", &self.extra_work.is_some())
            .field("defer_snapshot_reap", &self.defer_snapshot_reap)
            .finish()
    }
}

/// Append one event, and publish it if it committed and the fence allows.
///
/// This is the only durable write in the coordinator. Every caller reaches it
/// through `EventLog::append_event`, and the worker link reaches it with the
/// connection's authenticated fingerprint -- never with a fingerprint the frame
/// claimed.
pub async fn append_event(
    database: &CoordDb,
    event: SessionEvent,
    caller: &Caller,
    options: &mut AppendOptions<'_>,
) -> Result<AppendEventResult, AppendError> {
    let event = prepare_event(event)?;
    reserve_publication_slot(options, caller)?;

    let mut transaction = database.pool().begin().await?;
    let committed = match run_in_transaction(&mut transaction, event, caller, options).await {
        Ok(committed) => committed,
        Err(error) => {
            // v2 releases the reservation on a throw, so a failed append leaves no
            // slot behind for the retry to trip over.
            release_reservation(options, caller);
            return Err(error);
        }
    };
    // The commit is the boundary. Nothing below this line can be reached with an
    // uncommitted event, and nothing above it can publish.
    transaction.commit().await?;

    let deduplicated = !committed.admission_rejected && committed.inserted_id.is_none();
    let committed_effect = committed.publication_effect(caller.worker_fp.clone());
    // The store is locked here and nowhere else: the publication step is
    // synchronous, so its guard is never held across an await.
    let mut store = lock_store(options.pending_publications.as_ref());
    let resolution = resolve_publication(
        committed_effect,
        deduplicated,
        &committed.event_json,
        caller,
        store.as_deref_mut(),
        LivePublication {
            buses: options.buses,
            live_effects: options.live_effects,
            can_publish: options.can_publish,
        },
    )?;
    drop(store);
    Ok(build_result(&committed, &resolution, options))
}

/// Lock the publication store, or take nothing when the process has none.
///
/// A poisoned lock is recovered rather than propagated: the only panics that can
/// happen inside it are allocation failures in a `HashMap`, and one of those must
/// not make every later event in the process fail.
fn lock_store(
    store: Option<&Arc<Mutex<PendingPublicationStore>>>,
) -> Option<std::sync::MutexGuard<'_, PendingPublicationStore>> {
    store.map(|store| store.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Assemble the caller's answer, and reap the orphans a snapshot gave back.
fn build_result(
    committed: &CommittedState,
    resolution: &PublicationResolution,
    options: &AppendOptions<'_>,
) -> AppendEventResult {
    let published_effect = resolution.published_effect.as_ref();
    let published = published_effect.is_some();
    let snapshot_reap_ids = published_effect
        .map(|effect| effect.snapshot_reap_ids.clone())
        .unwrap_or_else(|| committed.reap_orphan_ids.clone());

    // A reap is a kill sent to a worker over a socket that may not be the one that
    // announced the session, so a worker connection waits for its readiness
    // barrier. The durable effective snapshot has already omitted these ids, so a
    // failed best-effort kill cannot resurrect a route.
    if published
        && !options.defer_snapshot_reap
        && let Some(SessionEvent::Snapshot { worker_fp, .. }) =
            published_effect.map(|effect| &effect.event)
    {
        for session_id in &snapshot_reap_ids {
            options.live_effects.kill_orphan_pty(worker_fp, session_id);
        }
    }

    AppendEventResult {
        admitted: !committed.admission_rejected,
        inserted: committed.inserted_id.is_some(),
        published,
        replay_rejected: resolution.replay_rejected,
        event: published_effect
            .map(|effect| effect.event.clone())
            .unwrap_or_else(|| committed.event.clone()),
        snapshot_reap_ids,
    }
}
