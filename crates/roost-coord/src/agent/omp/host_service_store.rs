//! Durable identity for admitted semantic UI actions and their ephemeral
//! host-service requests. Only routing metadata is stored: never an action
//! payload, a clipboard value, or a URL.

use crate::db::CoordDb;
use sqlx::Row;

/// How long an admitted UI action keeps its origin identity. Dedupe outlives
/// any native action, and the row is pruned by the next action in the same
/// conversation.
pub(super) const UI_ADMISSION_TTL_MS: i64 = 60 * 60 * 1000;
/// One browser tab may wait on at most this many host-service prompts.
pub(super) const MAX_PENDING_HOST_SERVICES: i64 = 32;
/// A native prompt timeout is 30 seconds; this leaves room for a slow reply.
pub(super) const HOST_SERVICE_TTL_MS: i64 = 5 * 60 * 1000;

/// The authenticated origin of one admitted action, as the coordinator bound it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UiAdmission {
    pub fingerprint: String,
    pub client_id: String,
    pub tab_id: String,
    pub view_id: String,
    pub transport_id: String,
    pub worker_fp: String,
    pub receipt: bool,
    /// A terminal receipt was already delivered for this admitted action.
    pub receipt_consumed: bool,
}

/// The identity of one pending browser effect. `value` is absent by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingHostService {
    pub service_id: String,
    pub conversation_id: String,
    pub origin_request_id: String,
    pub fingerprint: String,
    pub client_id: String,
    pub tab_id: String,
    pub view_id: String,
    pub transport_id: String,
    pub worker_fp: String,
    pub kind: String,
}

/// The identity a host reply must present to consume a pending effect.
#[derive(Debug, Clone, Copy)]
pub(super) struct PendingKey<'a> {
    pub service_id: &'a str,
    pub conversation_id: &'a str,
    pub fingerprint: &'a str,
    pub tab_id: &'a str,
    pub view_id: &'a str,
    pub transport_id: &'a str,
}

/// The worker-generated semantic UI token currently persisted, empty when no
/// view is open.
pub(super) async fn current_transport(
    db: &CoordDb,
    conversation_id: &str,
) -> Result<String, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT omp_ui_transport_id FROM omp_sessions WHERE conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(db.pool())
    .await?
    .unwrap_or_default())
}

/// Whether a dashboard capability still names the live native view.
///
/// The capability is a bearer URL, so this is the one check that revokes it
/// when the session closes or its worker changes: both rotate the persisted
/// transport, and a stale transport must not keep a page alive.
pub(super) async fn dashboard_binding_current(
    db: &CoordDb,
    conversation_id: &str,
    worker_fp: &str,
    transport_id: &str,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM omp_sessions s JOIN agent_conversations c \
         ON c.id=s.conversation_id WHERE s.conversation_id=$1 AND c.worker_fp=$2 \
         AND s.omp_ui_transport_id=$3 AND s.omp_ui_transport_id<>''",
    )
    .bind(conversation_id)
    .bind(worker_fp)
    .bind(transport_id)
    .fetch_one(db.pool())
    .await?;
    Ok(found > 0)
}

/// Consume an admission's terminal receipt exactly once.
///
/// Receipt values are `0` (not tracked), `1` (tracked, owed) and `2`
/// (delivered). Only a `0`/`1` row can transition, so a replayed worker frame
/// publishes nothing.
pub(super) async fn consume_receipt_admission(
    db: &CoordDb,
    conversation_id: &str,
    request_id: &str,
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE omp_inputs SET receipt=2 WHERE conversation_id=$1 AND request_id=$2 \
         AND kind='ui' AND expires_ms>$3 AND receipt IN (0,1)",
    )
    .bind(conversation_id)
    .bind(request_id)
    .bind(now_ms)
    .execute(db.pool())
    .await?
    .rows_affected()
        == 1)
}

/// The canonical chat sequence the conversation's transcript has committed.
pub(super) async fn transcript_cursor(
    db: &CoordDb,
    conversation_id: &str,
) -> Result<u64, sqlx::Error> {
    let cursor = sqlx::query_scalar::<_, i64>(
        "SELECT transcript_cursor FROM omp_sessions WHERE conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(db.pool())
    .await?
    .unwrap_or(0);
    Ok(u64::try_from(cursor).unwrap_or(0))
}

/// Admit one action request id for a conversation, recording its origin.
///
/// Returns `false` when the id was already admitted, which is the dedupe path.
pub(super) async fn admit_ui_action(
    db: &CoordDb,
    conversation_id: &str,
    request_id: &str,
    admission: &UiAdmission,
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    let mut transaction = db.begin_write().await?;
    sqlx::query(
        "DELETE FROM omp_inputs WHERE conversation_id=$1 AND kind='ui' \
         AND expires_ms>0 AND expires_ms<=$2",
    )
    .bind(conversation_id)
    .bind(now_ms)
    .execute(&mut *transaction)
    .await?;
    let inserted = sqlx::query(
        "INSERT INTO omp_inputs(conversation_id,request_id,kind,created_ms,fingerprint,client_id,\
         tab_id,view_id,transport_id,worker_fp,receipt,expires_ms) \
         VALUES($1,$2,'ui',$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT DO NOTHING",
    )
    .bind(conversation_id)
    .bind(request_id)
    .bind(now_ms)
    .bind(&admission.fingerprint)
    .bind(&admission.client_id)
    .bind(&admission.tab_id)
    .bind(&admission.view_id)
    .bind(&admission.transport_id)
    .bind(&admission.worker_fp)
    .bind(if admission.receipt { 1_i64 } else { 0_i64 })
    .bind(now_ms.saturating_add(UI_ADMISSION_TTL_MS))
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;
    transaction.commit().await?;
    Ok(inserted)
}

/// The origin bound to an admitted action request, when it is still valid.
pub(super) async fn ui_admission(
    db: &CoordDb,
    conversation_id: &str,
    request_id: &str,
    now_ms: i64,
) -> Result<Option<UiAdmission>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT fingerprint,client_id,tab_id,view_id,transport_id,worker_fp,receipt \
         FROM omp_inputs WHERE conversation_id=$1 AND request_id=$2 AND kind='ui' \
         AND expires_ms>$3",
    )
    .bind(conversation_id)
    .bind(request_id)
    .bind(now_ms)
    .fetch_optional(db.pool())
    .await?;
    row.map(|row| {
        Ok(UiAdmission {
            fingerprint: row.try_get("fingerprint")?,
            client_id: row.try_get("client_id")?,
            tab_id: row.try_get("tab_id")?,
            view_id: row.try_get("view_id")?,
            transport_id: row.try_get("transport_id")?,
            worker_fp: row.try_get("worker_fp")?,
            receipt: row.try_get::<i64, _>("receipt")? == 1,
            receipt_consumed: row.try_get::<i64, _>("receipt")? == 2,
        })
    })
    .transpose()
}

/// Record one pending effect, unless the conversation is already at its bound.
///
/// Returns `false` for a duplicate service id or a full conversation. The row
/// holds identity only, so a dropped request can never resurface as a prompt.
pub(super) async fn insert_pending(
    db: &CoordDb,
    pending: &PendingHostService,
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    let mut transaction = db.begin_write().await?;
    if db.backend() == crate::db::DbBackend::Postgres {
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('omp-host-services:' || $1, 0))",
        )
        .bind(&pending.conversation_id)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query("DELETE FROM omp_host_services WHERE conversation_id=$1 AND expires_ms<=$2")
        .bind(&pending.conversation_id)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM omp_host_services WHERE conversation_id=$1",
    )
    .bind(&pending.conversation_id)
    .fetch_one(&mut *transaction)
    .await?;
    if count >= MAX_PENDING_HOST_SERVICES {
        transaction.rollback().await?;
        return Ok(false);
    }
    let inserted = sqlx::query(
        "INSERT INTO omp_host_services(service_id,conversation_id,origin_request_id,fingerprint,\
         client_id,tab_id,view_id,transport_id,worker_fp,kind,created_ms,expires_ms) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT DO NOTHING",
    )
    .bind(&pending.service_id)
    .bind(&pending.conversation_id)
    .bind(&pending.origin_request_id)
    .bind(&pending.fingerprint)
    .bind(&pending.client_id)
    .bind(&pending.tab_id)
    .bind(&pending.view_id)
    .bind(&pending.transport_id)
    .bind(&pending.worker_fp)
    .bind(&pending.kind)
    .bind(now_ms)
    .bind(now_ms.saturating_add(HOST_SERVICE_TTL_MS))
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;
    transaction.commit().await?;
    Ok(inserted)
}

/// Consume one pending effect for exactly the identity that raised it.
///
/// The delete is conditional on the same identity and the live native token,
/// so a concurrent duplicate reply deletes nothing and receives `None`.
pub(super) async fn consume_pending(
    db: &CoordDb,
    key: &PendingKey<'_>,
    now_ms: i64,
) -> Result<Option<PendingHostService>, sqlx::Error> {
    let mut transaction = db.begin_write().await?;
    let row = sqlx::query(
        "SELECT p.service_id,p.conversation_id,p.origin_request_id,p.fingerprint,p.client_id,\
         p.tab_id,p.view_id,p.transport_id,p.worker_fp,p.kind \
         FROM omp_host_services p \
         WHERE p.service_id=$1 AND p.conversation_id=$2 AND p.fingerprint=$3 \
         AND p.tab_id=$4 AND p.view_id=$5 AND p.transport_id=$6 AND p.expires_ms>$7 \
         AND EXISTS(SELECT 1 FROM omp_sessions s WHERE s.conversation_id=$2 \
         AND s.omp_ui_transport_id=$6)",
    )
    .bind(key.service_id)
    .bind(key.conversation_id)
    .bind(key.fingerprint)
    .bind(key.tab_id)
    .bind(key.view_id)
    .bind(key.transport_id)
    .bind(now_ms)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = row else {
        transaction.rollback().await?;
        return Ok(None);
    };
    let pending = PendingHostService {
        service_id: row.try_get("service_id")?,
        conversation_id: row.try_get("conversation_id")?,
        origin_request_id: row.try_get("origin_request_id")?,
        fingerprint: row.try_get("fingerprint")?,
        client_id: row.try_get("client_id")?,
        tab_id: row.try_get("tab_id")?,
        view_id: row.try_get("view_id")?,
        transport_id: row.try_get("transport_id")?,
        worker_fp: row.try_get("worker_fp")?,
        kind: row.try_get("kind")?,
    };
    let deleted = sqlx::query(
        "DELETE FROM omp_host_services WHERE service_id=$1 AND conversation_id=$2 \
         AND fingerprint=$3 AND tab_id=$4 AND view_id=$5 AND transport_id=$6",
    )
    .bind(key.service_id)
    .bind(key.conversation_id)
    .bind(key.fingerprint)
    .bind(key.tab_id)
    .bind(key.view_id)
    .bind(key.transport_id)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    if deleted == 1 {
        transaction.commit().await?;
        Ok(Some(pending))
    } else {
        transaction.rollback().await?;
        Ok(None)
    }
}

/// Withdraw one pending effect for a conversation. Returns whether one was
/// still pending, so a duplicate cancellation is not fanned out twice.
pub(super) async fn delete_pending(
    db: &CoordDb,
    service_id: &str,
    conversation_id: &str,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM omp_host_services WHERE service_id=$1 AND conversation_id=$2")
            .bind(service_id)
            .bind(conversation_id)
            .execute(db.pool())
            .await?
            .rows_affected()
            == 1,
    )
}
