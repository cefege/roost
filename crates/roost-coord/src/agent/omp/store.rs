//! OMP conversation rows and their durable projected transcript.
//! The OMP service and RPCs use these queries; native rows are filtered out.
//! Settings and catalogs share this database but never provider credentials.

pub use super::settings::{catalogs, put_catalog, put_settings, settings};
use crate::db::CoordDb;
use roost_protocol::wire::agent_chat::*;
use sqlx::Row;

#[derive(Debug, Clone)]
pub struct OmpConversation {
    pub id: String,
    pub title: String,
    pub worker_fp: String,
    pub worker_label: String,
    pub cwd: String,
    pub override_values: OmpSettingValues,
    pub mode: Option<String>,
    pub run_state: AgentRunState,
    pub error: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// ACP `loadSession` identifier for recovery, not the live worker session handle.
    pub acp_session_id: Option<String>,
    pub current_model: Option<String>,
    pub current_thinking_level: Option<String>,
    pub applied_title: Option<String>,
    pub run_id: Option<String>,
    pub applied_seq: u64,
    pub worker_epoch: Option<String>,
}

#[derive(Debug)]
pub struct OmpNew {
    pub worker_fp: String,
    pub worker_label: String,
    pub worker_os: String,
    pub cwd: String,
}

pub async fn create(db: &CoordDb, new: OmpNew) -> Result<OmpConversation, sqlx::Error> {
    let id = super::super::random_hex("c-");
    let now = super::super::now_ms();
    let mut transaction = super::super::chat_events::begin_chat_write(db).await?;
    sqlx::query("INSERT INTO agent_conversations(id,title,worker_fp,worker_label,worker_os,cwd,thinking_level,mode,read_only,run_state,created_ms,updated_ms,revision,backend) VALUES($1,'',$2,$3,$4,$5,'','normal',$7,'idle',$6,$6,1,'omp')")
        .bind(&id).bind(new.worker_fp).bind(new.worker_label).bind(new.worker_os).bind(new.cwd).bind(now).bind(false)
        .execute(&mut *transaction).await?;
    sqlx::query("INSERT INTO omp_sessions(conversation_id,transcript_json) VALUES($1,$2)")
        .bind(&id)
        .bind(encode(&empty_transcript())?)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    find(db, &id).await?.ok_or(sqlx::Error::RowNotFound)
}

pub async fn find(db: &CoordDb, id: &str) -> Result<Option<OmpConversation>, sqlx::Error> {
    let row = sqlx::query("SELECT c.*,s.acp_session_id,s.current_model,s.current_thinking_level,s.applied_title,s.run_id,s.applied_seq,s.worker_epoch FROM agent_conversations c JOIN omp_sessions s ON s.conversation_id=c.id WHERE c.id=$1 AND c.backend='omp'")
        .bind(id).fetch_optional(db.pool()).await?;
    row.as_ref().map(decode).transpose()
}

pub async fn list(db: &CoordDb) -> Result<Vec<OmpConversation>, sqlx::Error> {
    sqlx::query("SELECT c.*,s.acp_session_id,s.current_model,s.current_thinking_level,s.applied_title,s.run_id,s.applied_seq,s.worker_epoch FROM agent_conversations c JOIN omp_sessions s ON s.conversation_id=c.id WHERE c.backend='omp' ORDER BY c.updated_ms DESC,c.id")
        .fetch_all(db.pool()).await?.iter().map(decode).collect()
}

pub async fn set_title(db: &CoordDb, id: &str, title: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE agent_conversations SET title=$2,updated_ms=$3 WHERE id=$1 AND backend='omp'",
    )
    .bind(id)
    .bind(title)
    .bind(super::super::now_ms())
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn set_override(
    db: &CoordDb,
    id: &str,
    model: Option<Option<String>>,
    thinking: Option<Option<String>>,
    mode: Option<Option<String>>,
) -> Result<(), sqlx::Error> {
    let mut transaction = super::super::chat_events::begin_chat_write(db).await?;
    if let Some(value) = model {
        let model = value.as_deref().and_then(split_omp_model);
        sqlx::query("UPDATE agent_conversations SET model_provider=$2,model_id=$3,updated_ms=$4 WHERE id=$1 AND backend='omp'")
            .bind(id).bind(model.as_ref().map(|model| &model.provider)).bind(model.as_ref().map(|model| &model.model_id))
            .bind(super::super::now_ms()).execute(&mut *transaction).await?;
    }
    if let Some(value) = thinking {
        sqlx::query("UPDATE agent_conversations SET thinking_level=$2,updated_ms=$3 WHERE id=$1 AND backend='omp'")
            .bind(id).bind(value.unwrap_or_default()).bind(super::super::now_ms()).execute(&mut *transaction).await?;
    }
    if let Some(value) = mode {
        sqlx::query(
            "UPDATE agent_conversations SET mode=$2,updated_ms=$3 WHERE id=$1 AND backend='omp'",
        )
        .bind(id)
        .bind(value.unwrap_or_default())
        .bind(super::super::now_ms())
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await
}

pub async fn delete(db: &CoordDb, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM agent_conversations WHERE id=$1 AND backend='omp'")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn admit_input(
    db: &CoordDb,
    id: &str,
    request_id: &str,
    kind: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("INSERT INTO omp_inputs(conversation_id,request_id,kind,created_ms) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(id).bind(request_id).bind(kind).bind(super::super::now_ms()).execute(db.pool()).await?.rows_affected() == 1)
}

pub async fn has_prompt_admission(
    db: &CoordDb,
    id: &str,
    request_id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM omp_inputs WHERE conversation_id=$1 AND request_id=$2 AND kind='prompt'",
    )
    .bind(id)
    .bind(request_id)
    .fetch_one(db.pool())
    .await.map(|count| count != 0)
}
pub async fn remove_admission(db: &CoordDb, id: &str, request_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM omp_inputs WHERE conversation_id=$1 AND request_id=$2")
        .bind(id)
        .bind(request_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn snapshot(db: &CoordDb, id: &str) -> Result<(u64, Transcript), sqlx::Error> {
    let (cursor, json): (i64, String) = sqlx::query_as(
        "SELECT transcript_cursor,transcript_json FROM omp_sessions WHERE conversation_id=$1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await?;
    Ok((
        cursor as u64,
        serde_json::from_str(&json).map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
    ))
}

pub fn empty_transcript() -> Transcript {
    Transcript {
        items: vec![],
        run_state: AgentRunState::Idle,
        error: None,
        model: None,
        thinking_level: None,
        mode: None,
        omp_session: None,
        usage: UsageTotals::default(),
        queued: vec![],
        omp_commands: vec![],
        omp_interactions: vec![],
        omp_ui: None,
        omp_ui_transport_id: None,
        omp_ui_error: None,
        schedules: vec![],
    }
}

pub(super) fn encode(value: &impl serde::Serialize) -> Result<String, sqlx::Error> {
    serde_json::to_string(value).map_err(|error| sqlx::Error::Encode(Box::new(error)))
}

fn decode(row: &sqlx::any::AnyRow) -> Result<OmpConversation, sqlx::Error> {
    let provider: Option<String> = row.try_get("model_provider")?;
    let model: Option<String> = row.try_get("model_id")?;
    let thinking: String = row.try_get("thinking_level")?;
    Ok(OmpConversation {
        id: row.try_get("id")?,
        title: row.try_get("title")?,
        worker_fp: row.try_get("worker_fp")?,
        worker_label: row.try_get("worker_label")?,
        cwd: row.try_get("cwd")?,
        override_values: OmpSettingValues {
            model: provider
                .zip(model)
                .map(|(provider, model)| format!("{provider}/{model}")),
            thinking_level: (!thinking.is_empty()).then_some(thinking),
        },
        mode: {
            let mode: String = row.try_get("mode")?;
            (!mode.is_empty()).then_some(mode)
        },
        run_state: AgentRunState::from_wire(&row.try_get::<String, _>("run_state")?),
        error: row.try_get("error")?,
        created_ms: row.try_get("created_ms")?,
        updated_ms: row.try_get("updated_ms")?,
        acp_session_id: row.try_get("acp_session_id")?,
        current_model: row.try_get("current_model")?,
        current_thinking_level: row.try_get("current_thinking_level")?,
        applied_title: row.try_get("applied_title")?,
        run_id: row.try_get("run_id")?,
        applied_seq: row.try_get::<i64, _>("applied_seq")? as u64,
        worker_epoch: row.try_get("worker_epoch")?,
    })
}
