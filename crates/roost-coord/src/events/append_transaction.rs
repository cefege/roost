//! The transaction body of the append path, and nothing that happens after it.
//!
//! Ported from `apps/coord/src/events/event-transaction.ts:76-283`. This module
//! cannot publish: the publisher is private to `append_publication` and takes
//! committed state the caller does not hold until `commit()` has resolved. That
//! is the whole point of the split -- `docs/phase3-coord-contract.md` §3.2
//! records that v2 enforces "publish strictly after commit" structurally, and a
//! port that put the bus call in this file would have the same shape and none of
//! the guarantee.
//!
//! The order inside is the contract and is not rearranged:
//!
//! 1. **admission**, whose refusal writes nothing;
//! 2. the `workspace_assigned` availability throw, which rolls back;
//! 3. the snapshot tombstone filter, applied to the *effective* event;
//! 4. the caller's atomic extra work -- after admission, so a foreign request
//!    cannot make an auxiliary mutation;
//! 5. the insert, with the dedupe clause;
//! 6. the dedupe short-circuit, which skips the projection and the publication
//!    and still acknowledges;
//! 7. the projection arm, which is the only thing that sets `publishable`.

use roost_protocol::wire::{SessionEvent, SessionId, SessionMap, WorkerFp, WorkspaceId};
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::sqlite::SqliteConnection;

use crate::events::admission::{Admission, admit};
use crate::events::admission_facts::load_admission_facts;
use crate::events::agent_conversation_recovery::project_agent_conversation_reference;
use crate::events::append::{AppendError, AppendOptions, Caller};
use crate::events::append_input::{as_client_seq, as_event_id};
use crate::events::append_publication::CommittedEventPublication;
use crate::events::projection_writes::{
    cascade_closed_session, delete_session, fold_and_update_session, insert_opened_session,
    project_snapshot_sessions, set_workspace_membership,
};

/// What the transaction decided, about the event it decided on.
#[derive(Debug)]
pub(crate) struct CommittedState {
    /// The `events.id` this call inserted, absent on a dedupe or a refusal.
    pub(crate) inserted_id: Option<u64>,
    /// The exact JSON that was persisted, kept for the dedupe comparison.
    pub(crate) event_json: String,
    /// Whether the committed event may reach the channel index and the bus.
    pub(crate) publishable: bool,
    /// Whether admission refused, or an `opened` lost its insert race.
    pub(crate) admission_rejected: bool,
    /// Sessions a snapshot found force-closed, to be killed after commit.
    pub(crate) reap_orphan_ids: Vec<String>,
    /// Workspaces a `closed` orphaned, to be published after commit.
    pub(crate) cascade_orphan_ids: Vec<WorkspaceId>,
    /// The effective event: normalized, and tombstone-filtered.
    pub(crate) event: SessionEvent,
}

impl CommittedState {
    /// The publishable effect, when there is one.
    ///
    /// A dedupe hit and a fold that produced no row leave this `None`, and neither
    /// may reach the channel index or the bus. The decision is taken here, inside
    /// the transaction, rather than after it (`event-transaction.ts:113-117`).
    pub(crate) fn publication_effect(
        &self,
        authenticated_worker_fp: Option<WorkerFp>,
    ) -> Option<CommittedEventPublication> {
        if !self.publishable {
            return None;
        }
        let event_id = self.inserted_id?;
        Some(CommittedEventPublication {
            event: self.event.clone(),
            authenticated_worker_fp,
            event_id,
            event_json: self.event_json.clone(),
            cascade_orphan_ids: self.cascade_orphan_ids.clone(),
            snapshot_reap_ids: self.reap_orphan_ids.clone(),
        })
    }
}

/// The transaction body. It contains no publish call, and cannot.
pub(crate) async fn run_in_transaction(
    connection: &mut SqliteConnection,
    event: SessionEvent,
    caller: &Caller,
    options: &mut AppendOptions<'_>,
) -> Result<CommittedState, AppendError> {
    let mut state = CommittedState {
        inserted_id: None,
        event_json: String::new(),
        publishable: false,
        admission_rejected: false,
        reap_orphan_ids: Vec::new(),
        cascade_orphan_ids: Vec::new(),
        event,
    };

    let facts = load_admission_facts(
        connection,
        &state.event,
        caller.worker_fp.as_ref(),
        caller.client_seq,
    )
    .await?;
    let admission = admit(&facts);
    if !admission.admitted {
        state.admission_rejected = true;
        return Ok(state);
    }

    require_workspace(connection, &state.event).await?;
    let tombstoned = force_closed_ids(connection, &state.event).await?;
    if !tombstoned.is_empty() {
        state.reap_orphan_ids.clone_from(&tombstoned);
        if let SessionEvent::Snapshot { sessions, .. } = &mut state.event {
            sessions.retain(|session| !tombstoned.contains(&session.id.to_string()));
        }
    }

    if let Some(extra_work) = options.extra_work.as_mut() {
        extra_work(connection)
            .await
            .map_err(AppendError::ExtraWork)?;
    }

    state.event_json = serde_json::to_string(&state.event)?;
    state.inserted_id = insert_event(connection, &state, caller).await?;

    if state.inserted_id.is_none() && caller.client_seq.is_some() {
        roost_observability::log::debug(
            "events.append",
            "dedup_hit",
            roost_observability::LogFields::new()
                .set("worker_fp", caller.worker_fp.as_ref().map(WorkerFp::as_str))
                .set("client_seq", caller.client_seq),
        );
        return Ok(state);
    }

    project(connection, &mut state, &admission, caller, options).await?;
    Ok(state)
}

/// The insert, and the one clause the whole at-least-once story rests on.
///
/// `ON CONFLICT (worker_fp, client_seq) WHERE ... DO NOTHING` targets the
/// *partial* unique index `events_worker_client_seq`, so a NULL fingerprint or
/// sequence -- a coordinator-side producer -- never collides with anything, and a
/// worker delivery that repeats a sequence no-ops instead of raising. The
/// migration header states the intent: "Coord acks every successful insert OR
/// successful dedup so the worker drops the event from its unacked outbox." Drop
/// the clause and every redelivered `WSessionEvent` becomes a second durable
/// event: a `closed` lands twice and a browser folds two closures for one session.
async fn insert_event(
    connection: &mut SqliteConnection,
    state: &CommittedState,
    caller: &Caller,
) -> Result<Option<u64>, AppendError> {
    let client_seq = caller.client_seq.map(as_client_seq).transpose()?;
    let inserted = sqlx::query_scalar::<_, i64>(
        "INSERT INTO events (dashboard_id, kind, session_id, worker_fp, payload_json, ts, client_seq) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (worker_fp, client_seq) \
           WHERE worker_fp IS NOT NULL AND client_seq IS NOT NULL DO NOTHING \
         RETURNING id",
    )
    .bind(&caller.dashboard_id)
    .bind(state.event.kind_name())
    .bind(state.event.session_id().map(SessionId::as_str))
    .bind(caller.worker_fp.as_ref().map(WorkerFp::as_str))
    .bind(&state.event_json)
    .bind(state.event.ts())
    .bind(client_seq)
    .fetch_optional(&mut *connection)
    .await?;
    inserted
        .map(|id| u64::try_from(id).map_err(|_| AppendError::EventIdOutOfRange { id }))
        .transpose()
}

/// A `workspace_assigned` naming a missing workspace throws, and rolls back.
///
/// Deliberately harsher than admission: the workspace does not exist, so a retry
/// cannot help and a silent refusal would leave the session's junction pointing at
/// nothing (`event-transaction.ts:123-131`).
async fn require_workspace(
    connection: &mut SqliteConnection,
    event: &SessionEvent,
) -> Result<(), AppendError> {
    let SessionEvent::WorkspaceAssigned {
        workspace_id: Some(workspace_id),
        ..
    } = event
    else {
        return Ok(());
    };
    let row = sqlx::query("SELECT 1 FROM workspaces WHERE id = ?")
        .bind(workspace_id.as_str())
        .fetch_optional(&mut *connection)
        .await?;
    if row.is_none() {
        return Err(AppendError::WorkspaceUnavailable);
    }
    Ok(())
}

/// The sessions a prior durable `closed` has permanently force-closed.
///
/// A `closed` is a tombstone, not a status: a returning worker that re-announces a
/// force-closed session in its snapshot must not resurrect it. The filter runs
/// before the event is serialized, so the log, the projection, the route index and
/// the Sync publication all see the same set (`event-transaction.ts:132-152`).
async fn force_closed_ids(
    connection: &mut SqliteConnection,
    event: &SessionEvent,
) -> Result<Vec<String>, AppendError> {
    let SessionEvent::Snapshot { sessions, .. } = event else {
        return Ok(Vec::new());
    };
    if sessions.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT session_id FROM events WHERE kind = 'closed' AND session_id IN (",
    );
    {
        let mut separated = query.separated(", ");
        for session in sessions {
            separated.push_bind(session.id.as_str());
        }
        separated.push_unseparated(")");
    }
    let rows = query.build().fetch_all(&mut *connection).await?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect())
}

/// The projection arm for the event's kind, and the only place `publishable` is
/// decided.
async fn project(
    connection: &mut SqliteConnection,
    state: &mut CommittedState,
    admission: &Admission,
    caller: &Caller,
    options: &AppendOptions<'_>,
) -> Result<(), AppendError> {
    match &state.event {
        SessionEvent::Snapshot { sessions, .. } => {
            project_snapshot_sessions(connection, sessions, &caller.dashboard_id).await?;
            state.publishable = true;
            return Ok(());
        }
        SessionEvent::AgentReference {
            session_id,
            reference,
            ..
        } => {
            let (Some(worker_fp), Some(client_seq)) =
                (caller.worker_fp.as_ref(), caller.client_seq)
            else {
                return Err(AppendError::AgentReferenceNeedsWorkerDelivery);
            };
            project_agent_conversation_reference(
                connection,
                session_id,
                reference.as_ref(),
                client_seq,
                worker_fp,
            )
            .await?;
            // The durable row and the private recovery projection commit, but this
            // event deliberately has no channel-index or browser publication
            // effect.
            return Ok(());
        }
        SessionEvent::Opened { session_id, .. } => {
            let folded = roost_protocol::wire::fold_event(&SessionMap::new(), &state.event);
            let Some(session) = folded.get(session_id).cloned() else {
                return Ok(());
            };
            let won = insert_opened_session(connection, &session, &caller.dashboard_id).await?;
            if !admission.session_exists && !won {
                // A lost race: another append created the row first. This call
                // deletes its own `events` row so the loser leaves no phantom log
                // entry, and reports the refusal.
                if let Some(event_id) = state.inserted_id {
                    sqlx::query("DELETE FROM events WHERE id = ?")
                        .bind(as_event_id(event_id)?)
                        .execute(&mut *connection)
                        .await?;
                }
                state.inserted_id = None;
                state.admission_rejected = true;
                return Ok(());
            }
        }
        SessionEvent::Closed { session_id, .. } => {
            // The terminal exited, or a post-undo kill fired: DELETE the row, do
            // not park it as status="closed". The cascade runs first because the
            // session's foreign key would erase the workspace ownership evidence.
            state.cascade_orphan_ids =
                cascade_closed_session(connection, session_id.as_str()).await?;
            delete_session(connection, session_id.as_str()).await?;
        }
        _ => {
            let Some(session_id) = state.event.session_id().cloned() else {
                return Ok(());
            };
            if fold_and_update_session(connection, &state.event, &caller.dashboard_id)
                .await?
                .is_none()
            {
                // Either the row is gone -- which admission rule 11 should already
                // have refused -- or the fold produced nothing for it. Neither is
                // publishable, and neither may publish.
                roost_observability::log::warn(
                    "events.append",
                    "session_not_found",
                    roost_observability::LogFields::new()
                        .set("kind", state.event.kind_name())
                        .set("session_id", session_id.as_str()),
                );
                return Ok(());
            }
            if let SessionEvent::WorkspaceAssigned { workspace_id, .. } = &state.event {
                set_workspace_membership(
                    connection,
                    session_id.as_str(),
                    workspace_id.as_ref().map(WorkspaceId::as_str),
                    &caller.dashboard_id,
                    options.now_ms,
                )
                .await?;
            }
        }
    }
    state.publishable = true;
    Ok(())
}
