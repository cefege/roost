//! The database reads that fill `AdmissionFacts`, and nothing else.
//!
//! The twelve admission *rules* are pure and already live in
//! `crate::events::admission`. This file is the other half of v2's
//! `resolveEventAdmission` (`apps/coord/src/events/event-admission.ts:32-123`):
//! the queries that answer each question the rules ask, in the order the rules
//! ask them, so a rule can be read and tested without a database.
//!
//! WHY THE ORDER OF THESE QUERIES IS THE SAME AS THE ORDER OF THE RULES. The
//! rules are cheapest-first on purpose -- a caller whose worker row is gone is
//! refused before any session lookup, so a tombstoned worker cannot use the
//! coordinator as a session-existence oracle by watching which read happens. A
//! reader that collected every fact up front would answer all of them for every
//! caller, which is the same oracle with extra steps. So each function below is
//! reached only when the rule before it passed.
//!
//! EVERY REFUSAL IS A DATA OUTCOME. Nothing in this file returns an error for a
//! missing or foreign id: those are `false`s in the facts, and
//! `admission::admit` turns them into a refusal the worker link answers with no
//! ACK and no close (`docs/phase3-coord-contract.md` §3.3).

use std::collections::HashMap;

use roost_protocol::wire::{Session, SessionEvent, WorkerFp};
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::sqlite::SqliteConnection;

use crate::events::admission::AdmissionFacts;
use crate::events::visibility::PRIVATE_SESSION_EVENT_KIND;

/// Read every fact the admission rules need for this event and caller.
///
/// The reads are issued in rule order and each one is skipped when an earlier
/// rule has already decided the answer, exactly as v2 returned early from each
/// branch (`event-admission.ts:35-122`).
pub async fn load_admission_facts(
    connection: &mut SqliteConnection,
    event: &SessionEvent,
    caller_worker_fp: Option<&WorkerFp>,
    client_seq: Option<u64>,
) -> Result<AdmissionFacts, sqlx::Error> {
    let session_id = event.session_id().cloned();
    let mut facts = AdmissionFacts {
        caller_worker_fp: caller_worker_fp.map(|fp| fp.to_string()),
        event_worker_fp: event_claimed_worker_fp(event),
        event_kind: event.kind_name().to_owned(),
        session_id: session_id.as_ref().map(|id| id.to_string()),
        ..AdmissionFacts::default()
    };

    // Rule 1: a non-worker producer is admitted after a single existence probe.
    let Some(caller) = caller_worker_fp else {
        facts.session_exists = match &session_id {
            Some(session_id) => session_row_exists(connection, session_id.as_str()).await?,
            None => false,
        };
        return Ok(facts);
    };

    // Rule 2.
    facts.caller_worker_live = worker_is_live(connection, caller.as_str()).await?;
    if !facts.caller_worker_live {
        return Ok(facts);
    }

    // Rule 4: a durable row for this sequence is what lets a retry reach the
    // claim path that publishes the effect whose publication was lost.
    if let Some(client_seq) = client_seq {
        facts.already_deduplicated =
            durable_delivery_exists(connection, caller.as_str(), client_seq).await?;
        if facts.already_deduplicated {
            return Ok(facts);
        }
    }

    if let SessionEvent::Snapshot { sessions, .. } = event {
        // Rules 5, 6 and 7: the snapshot's own claims, the rows those ids
        // already have, and the workspaces it names.
        facts.snapshot_event_worker_fps = sessions
            .iter()
            .map(|session| session.worker_fp.to_string())
            .collect();
        let announced_ids = announced_session_ids(sessions.iter().map(|row| row.id.as_str()));
        let rows = session_row_owners(connection, &announced_ids).await?;
        facts.snapshot_row_worker_fps = announced_ids
            .iter()
            .filter_map(|id| rows.get(id.as_str()).cloned())
            .collect();
        facts.snapshot_workspace_ids = announced_workspace_ids(sessions);
        facts.existing_snapshot_workspace_count =
            existing_workspace_count(connection, &facts.snapshot_workspace_ids).await?;
        return Ok(facts);
    }

    // Rule 8: an event with no session id is a snapshot, already handled.
    let Some(session_id) = session_id else {
        return Ok(facts);
    };

    // Rule 9.
    if let Some(owner) = session_row_owner(connection, session_id.as_str()).await? {
        facts.session_exists = true;
        facts.session_row_worker_fp = Some(owner);
        return Ok(facts);
    }

    // Rule 10, and only for the private kind: a reference queued before an
    // offline force-close must still be consumed, or it permanently blocks the
    // worker's ordered durable replay.
    if event.kind_name() == PRIVATE_SESSION_EVENT_KIND {
        facts.worker_has_prior_opened =
            worker_has_durable_opened(connection, caller.as_str(), session_id.as_str()).await?;
    }
    Ok(facts)
}

/// The `worker_fp` an `opened` or `snapshot` claims, and nothing else.
///
/// Only those two kinds may claim a worker, so no other variant reports one --
/// a `respawned` has no `worker_fp` field to claim, and inventing one from the
/// route cache is the mistake the durable-publication suite pins
/// (`apps/coord/tests/durable-publication.test.ts:98-107`).
fn event_claimed_worker_fp(event: &SessionEvent) -> Option<String> {
    match event {
        SessionEvent::Opened { worker_fp, .. } | SessionEvent::Snapshot { worker_fp, .. } => {
            Some(worker_fp.to_string())
        }
        _ => None,
    }
}

async fn worker_is_live(
    connection: &mut SqliteConnection,
    worker_fp: &str,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query("SELECT 1 FROM workers WHERE fp = ? AND deleted_at_ms IS NULL")
        .bind(worker_fp)
        .fetch_optional(&mut *connection)
        .await?;
    Ok(row.is_some())
}

async fn durable_delivery_exists(
    connection: &mut SqliteConnection,
    worker_fp: &str,
    client_seq: u64,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query("SELECT 1 FROM events WHERE worker_fp = ? AND client_seq = ?")
        .bind(worker_fp)
        .bind(as_sqlite_integer(client_seq))
        .fetch_optional(&mut *connection)
        .await?;
    Ok(row.is_some())
}

async fn session_row_exists(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query("SELECT 1 FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&mut *connection)
        .await?;
    Ok(row.is_some())
}

async fn session_row_owner(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query("SELECT worker_fp FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&mut *connection)
        .await?;
    Ok(row.map(|row| row.get::<String, _>(0)))
}

/// The owner of every existing row among the announced ids.
///
/// Ids with no row contribute nothing, which is what v2's
/// `currentRows.some(...)` did: the check is "is any row that exists owned by
/// somebody else", not "does every announced id exist".
async fn session_row_owners(
    connection: &mut SqliteConnection,
    announced_ids: &[String],
) -> Result<HashMap<String, String>, sqlx::Error> {
    if announced_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut query =
        QueryBuilder::<sqlx::Sqlite>::new("SELECT id, worker_fp FROM sessions WHERE id IN (");
    {
        let mut separated = query.separated(", ");
        for id in announced_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
    }
    let rows = query.build().fetch_all(&mut *connection).await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
        .collect())
}

async fn existing_workspace_count(
    connection: &mut SqliteConnection,
    workspace_ids: &[String],
) -> Result<usize, sqlx::Error> {
    if workspace_ids.is_empty() {
        return Ok(0);
    }
    let mut query = QueryBuilder::<sqlx::Sqlite>::new("SELECT id FROM workspaces WHERE id IN (");
    {
        let mut separated = query.separated(", ");
        for id in workspace_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
    }
    let rows = query.build().fetch_all(&mut *connection).await?;
    Ok(rows.len())
}

async fn worker_has_durable_opened(
    connection: &mut SqliteConnection,
    worker_fp: &str,
    session_id: &str,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        "SELECT 1 FROM events WHERE session_id = ? AND worker_fp = ? AND kind = 'opened'",
    )
    .bind(session_id)
    .bind(worker_fp)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(row.is_some())
}

/// The announced session ids, deduplicated, in first-seen order.
fn announced_session_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for id in ids {
        if !seen.iter().any(|kept| kept == id) {
            seen.push(id.to_owned());
        }
    }
    seen
}

/// The workspaces a snapshot names, deduplicated, in first-seen order. A session
/// with no workspace contributes nothing: an orphan is a legal announcement.
fn announced_workspace_ids(sessions: &[Session]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for session in sessions {
        let Some(workspace_id) = session.workspace_id.as_ref() else {
            continue;
        };
        if !seen.iter().any(|kept| kept == workspace_id.as_str()) {
            seen.push(workspace_id.to_string());
        }
    }
    seen
}

/// The `events.client_seq` column is a SQLite `INTEGER`, and SQLite's encoder has
/// no `u64`. A worker sequence past `i64::MAX` is representable on the wire and
/// not in the log; the append path refuses it before it gets here, so this
/// conversion only has to be total.
fn as_sqlite_integer(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
