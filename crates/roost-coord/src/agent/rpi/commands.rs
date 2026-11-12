//! Persists saved-session OPEN commands under the shared conversation lock.
//! Request identity is checked before allocating an ordered command sequence.
//! Delivery occurs only after commit and cannot remove a pending admission.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::wire::agent_chat::rpi::RpiCommandPayload;
use sqlx::Row;

use crate::coord_core::{CoordCore, worker_handle::WorkerHandle};

pub(super) async fn admit_open(
    core: &CoordCore,
    request: &roost_proto::AgentRpiSessionOpenRequest,
) -> Result<(Arc<WorkerHandle>, roost_proto::DRpiCommand), ConnectError> {
    let fingerprint: Option<String> = sqlx::query_scalar("SELECT c.worker_fp FROM agent_conversations c JOIN rpi_sessions s ON s.conversation_id=c.id WHERE c.id=$1 AND c.backend='rpi' AND s.deleted_ms IS NULL")
        .bind(&request.conversation_id).fetch_optional(core.services.db.pool()).await.map_err(database_error)?;
    let fingerprint = fingerprint.ok_or_else(not_found)?;
    let worker = super::rpc::worker(core, &fingerprint)?;
    let db = &core.services.db;
    let mut transaction = super::super::chat_events::begin_chat_write(db)
        .await
        .map_err(database_error)?;
    if db.backend() == crate::db::DbBackend::Postgres {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('chat:' || $1, 0))")
            .bind(&request.conversation_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
    }
    let row = sqlx::query("SELECT c.cwd,c.model_provider,c.model_id,c.thinking_level FROM agent_conversations c JOIN rpi_sessions s ON s.conversation_id=c.id WHERE c.id=$1 AND c.backend='rpi' AND s.deleted_ms IS NULL")
        .bind(&request.conversation_id).fetch_optional(&mut *transaction).await.map_err(database_error)?
        .ok_or_else(not_found)?;
    let existing = sqlx::query("SELECT command_seq,kind,payload_json FROM rpi_commands WHERE conversation_id=$1 AND request_id=$2")
        .bind(&request.conversation_id).bind(&request.request_id).fetch_optional(&mut *transaction).await.map_err(database_error)?;
    let (sequence, json) = if let Some(existing) = existing {
        let kind: String = existing.try_get("kind").map_err(database_error)?;
        let json: String = existing.try_get("payload_json").map_err(database_error)?;
        let payload: RpiCommandPayload =
            serde_json::from_str(&json).map_err(|_| storage_error())?;
        if kind != "open"
            || !matches!(payload, RpiCommandPayload::Open { resume_pending, .. } if resume_pending == request.resume_pending)
        {
            return Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "request_id already names a different command",
            ));
        }
        (
            existing
                .try_get::<i64, _>("command_seq")
                .map_err(database_error)?,
            json,
        )
    } else {
        let provider: Option<String> = row.try_get("model_provider").map_err(database_error)?;
        let model: Option<String> = row.try_get("model_id").map_err(database_error)?;
        let (Some(model_provider), Some(model_id)) = (provider, model) else {
            return Err(ConnectError::new(
                ErrorCode::FailedPrecondition,
                "RPI conversation has no model pin",
            ));
        };
        if model_provider.is_empty() || model_id.is_empty() {
            return Err(ConnectError::new(
                ErrorCode::FailedPrecondition,
                "RPI conversation has no model pin",
            ));
        }
        let payload = RpiCommandPayload::Open {
            cwd: row.try_get("cwd").map_err(database_error)?,
            model_provider,
            model_id,
            thinking_level: row.try_get("thinking_level").map_err(database_error)?,
            resume_pending: request.resume_pending,
        };
        let json = serde_json::to_string(&payload).map_err(|_| storage_error())?;
        let fingerprint = payload
            .semantic_fingerprint()
            .map_err(|_| storage_error())?;
        let sequence: i64 = sqlx::query_scalar("UPDATE rpi_sessions SET next_command_seq=next_command_seq+1 WHERE conversation_id=$1 AND deleted_ms IS NULL AND next_command_seq<9223372036854775807 RETURNING next_command_seq")
            .bind(&request.conversation_id).fetch_one(&mut *transaction).await.map_err(database_error)?;
        let now = super::super::now_ms();
        sqlx::query("INSERT INTO rpi_commands(conversation_id,request_id,command_seq,kind,payload_json,payload_fingerprint,delivery_state,created_ms,updated_ms) VALUES($1,$2,$3,'open',$4,$5,'pending',$6,$6)")
            .bind(&request.conversation_id).bind(&request.request_id).bind(sequence).bind(&json).bind(fingerprint).bind(now)
            .execute(&mut *transaction).await.map_err(database_error)?;
        (sequence, json)
    };
    transaction.commit().await.map_err(database_error)?;
    tracing::info!(conversation_id = %request.conversation_id, command_seq = sequence, resume_pending = request.resume_pending, "RPI session activation admitted");
    let command = roost_proto::DRpiCommand {
        conversation_id: request.conversation_id.clone(),
        request_id: request.request_id.clone(),
        command_seq: u64::try_from(sequence).map_err(|_| storage_error())?,
        expected_worker_epoch: worker.process_epoch.clone().unwrap_or_default(),
        kind: roost_proto::RpiCommandKind::Open.into(),
        payload_json: json,
        ..Default::default()
    };
    Ok((worker, command))
}

fn not_found() -> ConnectError {
    ConnectError::new(ErrorCode::NotFound, "conversation not found")
}

fn storage_error() -> ConnectError {
    ConnectError::new(ErrorCode::Internal, "RPI command storage invalid")
}

fn database_error(error: sqlx::Error) -> ConnectError {
    tracing::warn!(%error, "RPI command database failure");
    ConnectError::new(ErrorCode::Internal, "RPI command storage failed")
}
