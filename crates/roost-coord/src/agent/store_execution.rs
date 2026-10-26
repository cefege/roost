//! The replica-safe half of the agent store: command admission with request
//! dedup, run leases, fenced journal appends and orphaned-run discovery.
//! `store.rs` delegates its `AgentStore` methods here; every multi-statement
//! decision runs inside one `CoordDb::begin_write` transaction.

use roost_agent::admission::{
    LeaseRow, intent_entry, lease_expiry, next_lease_epoch, queued_inputs_to_activate,
    restart_boundary_matches, submission_is_withdrawable,
};
use roost_agent::error::AgentError;
use roost_agent::records::Entry;
use roost_agent::traits::{AdmitOutcome, CommandAdmission, IntentChange, LeaseFence};
use sqlx::Row as _;
use tokio_util::sync::CancellationToken;

use crate::db::{CoordDb, DbBackend};

type WriteTx<'c> = sqlx::Transaction<'c, sqlx::Any>;

pub(super) async fn append_entry(
    db: &CoordDb,
    id: &str,
    entry: &Entry,
    cancellation: &[&CancellationToken],
    fence: Option<&LeaseFence>,
) -> Result<u64, AgentError> {
    let json = serde_json::to_string(entry).map_err(serialize_error)?;
    let mut tx = db.begin_write().await.map_err(store_error)?;
    require_conversation(&mut tx, id).await?;
    if let Some(fence) = fence
        && !holds_lease(db.backend(), &mut tx, id, fence).await?
    {
        tx.rollback().await.map_err(store_error)?;
        return Err(AgentError::Fenced(id.to_owned()));
    }
    let seq = last_seq(&mut tx, id).await?.saturating_add(1);
    insert_entry(&mut tx, id, seq, &json).await?;
    if cancellation.iter().any(|token| token.is_cancelled()) {
        tx.rollback().await.map_err(store_error)?;
        return Err(AgentError::Cancelled);
    }
    tx.commit().await.map_err(store_error)?;
    Ok(seq)
}

pub(super) async fn admit_command(
    db: &CoordDb,
    id: &str,
    admission: CommandAdmission,
) -> Result<AdmitOutcome, AgentError> {
    let mut tx = db.begin_write().await.map_err(store_error)?;
    require_conversation(&mut tx, id).await?;
    let earlier: Option<Option<String>> = sqlx::query_scalar(
        "SELECT result_json FROM agent_commands WHERE conversation_id=$1 AND request_id=$2",
    )
    .bind(id)
    .bind(&admission.request_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(store_error)?;
    if let Some(result_json) = earlier {
        tx.rollback().await.map_err(store_error)?;
        return Ok(AdmitOutcome::Duplicate { result_json });
    }
    let rows = sqlx::query(
        "SELECT seq,entry_json FROM agent_entries WHERE conversation_id=$1 ORDER BY seq",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(store_error)?;
    let mut journal = Vec::with_capacity(rows.len());
    for row in rows {
        let sequence: i64 = row.try_get("seq").map_err(store_error)?;
        let entry_json: String = row.try_get("entry_json").map_err(store_error)?;
        let entry = serde_json::from_str(&entry_json).map_err(serialize_error)?;
        journal.push((u64::try_from(sequence).map_err(serialize_error)?, entry));
    }
    if matches!(
        admission.intent,
        IntentChange::Complete | IntentChange::RestartIfCurrent
    ) {
        let Some(fence) = admission.fence.as_ref() else {
            tx.rollback().await.map_err(store_error)?;
            return Err(AgentError::Fenced(id.to_owned()));
        };
        if !holds_lease(db.backend(), &mut tx, id, fence).await? {
            tx.rollback().await.map_err(store_error)?;
            return Err(AgentError::Fenced(id.to_owned()));
        }
    }
    if admission.intent == IntentChange::RestartIfCurrent
        && !restart_boundary_matches(&journal, admission.expected_intent_seq)
    {
        tx.rollback().await.map_err(store_error)?;
        return Ok(AdmitOutcome::Stopped);
    }
    if admission.intent == IntentChange::Complete {
        let expected_intent = admission.expected_intent_seq.unwrap_or(0);
        if journal.iter().any(|(sequence, entry)| {
            *sequence > expected_intent && matches!(entry, Entry::RunIntent { aborted: true, .. })
        }) {
            tx.rollback().await.map_err(store_error)?;
            return Ok(AdmitOutcome::Stopped);
        }
    }
    if admission.intent == IntentChange::Withdraw {
        let submission_id = admission.entries.iter().find_map(|entry| match entry {
            Entry::QueuedInputWithdrawn { submission_id } => Some(submission_id),
            _ => None,
        });
        if submission_id
            .is_none_or(|submission_id| !submission_is_withdrawable(&journal, submission_id))
        {
            tx.rollback().await.map_err(store_error)?;
            return Ok(AdmitOutcome::NotWithdrawable);
        }
    }
    let busy = journal
        .iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::RunIntent { active, .. } => Some(*active),
            _ => None,
        })
        .unwrap_or(false);
    if admission.intent == IntentChange::RejectIfBusy && busy {
        tx.rollback().await.map_err(store_error)?;
        return Ok(AdmitOutcome::Busy);
    }
    let queued = admission.intent == IntentChange::FollowUp && busy;
    let intent = match admission.intent {
        IntentChange::RejectIfBusy => IntentChange::Activate,
        IntentChange::FollowUp if busy => IntentChange::None,
        IntentChange::FollowUp => IntentChange::Activate,
        intent => intent,
    };
    let mut entries = admission.entries;
    if admission.intent == IntentChange::FollowUp && !busy {
        entries = entries
            .into_iter()
            .flat_map(|entry| {
                roost_agent::admission::activate_followup(entry)
                    .into_iter()
                    .flatten()
            })
            .collect();
    }
    if intent == IntentChange::Complete {
        entries.extend(queued_inputs_to_activate(&journal));
    }
    let last_before = last_seq(&mut tx, id).await?;
    let mut seq = last_before;
    let mut seqs = Vec::with_capacity(entries.len() + 1);
    let boundary = admission.completion_through.unwrap_or(last_before);
    let mut has_unconsumed = roost_agent::journal::effective_entries(&journal)
        .iter()
        .any(|(sequence, entry)| *sequence > boundary && matches!(entry, Entry::User { .. }));
    for entry in &entries {
        seq = seq.saturating_add(1);
        let json = serde_json::to_string(entry).map_err(serialize_error)?;
        insert_entry(&mut tx, id, seq, &json).await?;
        if seq > boundary && matches!(entry, Entry::User { .. }) {
            has_unconsumed = true;
        }
        seqs.push(seq);
    }
    let intent_entry = if admission.intent == IntentChange::Complete {
        Some(Entry::RunIntent {
            active: has_unconsumed,
            through_seq: boundary,
            aborted: false,
            yielded: false,
        })
    } else {
        intent_entry(intent, last_before, seqs.last().copied())
    };
    if let Some(intent_entry) = intent_entry {
        seq = seq.saturating_add(1);
        let json = serde_json::to_string(&intent_entry).map_err(serialize_error)?;
        insert_entry(&mut tx, id, seq, &json).await?;
        seqs.push(seq);
    }
    sqlx::query("INSERT INTO agent_commands (conversation_id,request_id,kind,result_json,created_ms) VALUES ($1,$2,$3,NULL,$4)")
        .bind(id)
        .bind(&admission.request_id)
        .bind(&admission.kind)
        .bind(super::now_ms())
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
    tx.commit().await.map_err(store_error)?;
    Ok(AdmitOutcome::Admitted { seqs, queued })
}

pub(super) async fn complete_command(
    db: &CoordDb,
    id: &str,
    request_id: &str,
    result_json: &str,
) -> Result<(), AgentError> {
    sqlx::query(
        "UPDATE agent_commands SET result_json=$1 WHERE conversation_id=$2 AND request_id=$3",
    )
    .bind(result_json)
    .bind(id)
    .bind(request_id)
    .execute(db.pool())
    .await
    .map_err(store_error)?;
    Ok(())
}

pub(super) async fn acquire_lease(
    db: &CoordDb,
    id: &str,
    owner: &str,
    ttl_ms: u64,
) -> Result<Option<u64>, AgentError> {
    let now = super::now_ms();
    let mut tx = db.begin_write().await.map_err(store_error)?;
    require_conversation(&mut tx, id).await?;
    let current = lease_row(db.backend(), &mut tx, id).await?;
    let Some(epoch) = next_lease_epoch(current.as_ref(), owner, now) else {
        tx.rollback().await.map_err(store_error)?;
        return Ok(None);
    };
    sqlx::query("INSERT INTO agent_run_leases (conversation_id,owner_instance,epoch,expires_ms) VALUES ($1,$2,$3,$4) \
        ON CONFLICT(conversation_id) DO UPDATE SET owner_instance=excluded.owner_instance,epoch=excluded.epoch,expires_ms=excluded.expires_ms")
        .bind(id)
        .bind(owner)
        .bind(to_i64(epoch)?)
        .bind(lease_expiry(now, ttl_ms))
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
    tx.commit().await.map_err(store_error)?;
    Ok(Some(epoch))
}

pub(super) async fn renew_lease(
    db: &CoordDb,
    id: &str,
    fence: &LeaseFence,
    ttl_ms: u64,
) -> Result<bool, AgentError> {
    let renewed = sqlx::query("UPDATE agent_run_leases SET expires_ms=$1 WHERE conversation_id=$2 AND owner_instance=$3 AND epoch=$4")
        .bind(lease_expiry(super::now_ms(), ttl_ms))
        .bind(id)
        .bind(&fence.owner)
        .bind(to_i64(fence.epoch)?)
        .execute(db.pool())
        .await
        .map_err(store_error)?;
    Ok(renewed.rows_affected() == 1)
}

pub(super) async fn release_lease(
    db: &CoordDb,
    id: &str,
    fence: &LeaseFence,
) -> Result<(), AgentError> {
    sqlx::query("UPDATE agent_run_leases SET expires_ms=0 WHERE conversation_id=$1 AND owner_instance=$2 AND epoch=$3")
        .bind(id)
        .bind(&fence.owner)
        .bind(to_i64(fence.epoch)?)
        .execute(db.pool())
        .await
        .map_err(store_error)?;
    Ok(())
}

pub(super) async fn orphaned_runs(db: &CoordDb, now_ms: i64) -> Result<Vec<String>, AgentError> {
    sqlx::query_scalar(
        "SELECT c.id FROM agent_conversations c LEFT JOIN agent_run_leases l ON l.conversation_id = c.id \
         WHERE (l.expires_ms > 0 AND l.expires_ms <= $1) \
            OR (c.run_state = 'running' AND (l.conversation_id IS NULL OR l.expires_ms <= $1)) \
         ORDER BY c.id",
    )
    .bind(now_ms)
    .fetch_all(db.pool())
    .await
    .map_err(store_error)
}

pub(super) async fn require_conversation(tx: &mut WriteTx<'_>, id: &str) -> Result<(), AgentError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_conversations WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(store_error)?;
    if count == 0 {
        return Err(AgentError::NotFound(id.to_owned()));
    }
    Ok(())
}

pub(super) async fn last_seq(tx: &mut WriteTx<'_>, id: &str) -> Result<u64, AgentError> {
    let last: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) FROM agent_entries WHERE conversation_id=$1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await
    .map_err(store_error)?;
    u64::try_from(last).map_err(serialize_error)
}

pub(super) async fn insert_entry(
    tx: &mut WriteTx<'_>,
    id: &str,
    seq: u64,
    json: &str,
) -> Result<(), AgentError> {
    sqlx::query(
        "INSERT INTO agent_entries (conversation_id,seq,entry_json,created_ms) VALUES ($1,$2,$3,$4)",
    )
    .bind(id)
    .bind(to_i64(seq)?)
    .bind(json)
    .bind(super::now_ms())
    .execute(&mut **tx)
    .await
    .map_err(store_error)?;
    Ok(())
}

async fn lease_row(
    backend: DbBackend,
    tx: &mut WriteTx<'_>,
    id: &str,
) -> Result<Option<LeaseRow>, AgentError> {
    let statement = match backend {
        DbBackend::Sqlite => {
            "SELECT owner_instance,epoch,expires_ms FROM agent_run_leases WHERE conversation_id=$1"
        }
        DbBackend::Postgres => {
            "SELECT owner_instance,epoch,expires_ms FROM agent_run_leases WHERE conversation_id=$1 FOR UPDATE"
        }
    };
    let row = sqlx::query(statement)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(store_error)?;
    row.map(|row| {
        let epoch: i64 = row.try_get("epoch").map_err(store_error)?;
        Ok(LeaseRow {
            owner: row.try_get("owner_instance").map_err(store_error)?,
            epoch: u64::try_from(epoch).map_err(serialize_error)?,
            expires_ms: row.try_get("expires_ms").map_err(store_error)?,
        })
    })
    .transpose()
}

pub(super) async fn holds_lease(
    backend: DbBackend,
    tx: &mut WriteTx<'_>,
    id: &str,
    fence: &LeaseFence,
) -> Result<bool, AgentError> {
    Ok(lease_row(backend, tx, id).await?.is_some_and(|row| {
        row.owner == fence.owner && row.epoch == fence.epoch && row.expires_ms > super::now_ms()
    }))
}

pub(super) fn to_i64(value: u64) -> Result<i64, AgentError> {
    i64::try_from(value).map_err(serialize_error)
}

fn store_error(error: sqlx::Error) -> AgentError {
    AgentError::Store(error.to_string())
}

fn serialize_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Store(error.to_string())
}
