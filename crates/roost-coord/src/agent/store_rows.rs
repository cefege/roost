//! Decoding coordinator conversation rows into harness records.
//!
//! Kept separate from store operations so SQL row conversion stays small and
//! consistent across SQLite and Postgres through sqlx's `Any` driver.

use roost_agent::error::AgentError;
use roost_agent::records::{ConversationRecord, Mode};
use roost_protocol::wire::agent_chat::{AgentRunState, ModelRef};
use sqlx::Row as _;

use crate::db::DbBackend;

pub(super) fn decode_record(
    row: &sqlx::any::AnyRow,
    backend: DbBackend,
) -> Result<ConversationRecord, AgentError> {
    let provider: Option<String> = row.try_get("model_provider").map_err(store_error)?;
    let model_id: Option<String> = row.try_get("model_id").map_err(store_error)?;
    let model = match (provider, model_id) {
        (Some(provider), Some(model_id)) => Some(ModelRef { provider, model_id }),
        (None, None) => None,
        _ => {
            return Err(AgentError::Store(
                "conversation model columns disagree".to_owned(),
            ));
        }
    };
    let pre_plan_json: Option<String> = row.try_get("pre_plan_model").map_err(store_error)?;
    let pre_plan_model = pre_plan_json
        .map(|json| {
            serde_json::from_str(&json).map_err(|error| AgentError::Store(error.to_string()))
        })
        .transpose()?;
    let advisor: Option<bool> = match backend {
        DbBackend::Sqlite => row
            .try_get::<Option<i64>, _>("advisor")
            .map(|value| value.map(|value| value != 0)),
        DbBackend::Postgres => row.try_get("advisor"),
    }
    .map_err(store_error)?;
    Ok(ConversationRecord {
        id: row.try_get("id").map_err(store_error)?,
        title: row.try_get("title").map_err(store_error)?,
        worker_fp: row.try_get("worker_fp").map_err(store_error)?,
        worker_label: row.try_get("worker_label").map_err(store_error)?,
        worker_os: row.try_get("worker_os").map_err(store_error)?,
        cwd: row.try_get("cwd").map_err(store_error)?,
        model,
        thinking_level: row.try_get("thinking_level").map_err(store_error)?,
        mode: Mode::from_wire(&row.try_get::<String, _>("mode").map_err(store_error)?),
        pre_plan_model,
        parent_id: row.try_get("parent_id").map_err(store_error)?,
        agent: row.try_get("agent").map_err(store_error)?,
        advisor,
        run_state: AgentRunState::from_wire(
            &row.try_get::<String, _>("run_state").map_err(store_error)?,
        ),
        error: row.try_get("error").map_err(store_error)?,
        created_ms: row.try_get("created_ms").map_err(store_error)?,
        updated_ms: row.try_get("updated_ms").map_err(store_error)?,
    })
}

fn store_error(error: sqlx::Error) -> AgentError {
    AgentError::Store(error.to_string())
}
