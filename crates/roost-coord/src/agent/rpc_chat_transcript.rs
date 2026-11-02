//! Device-authorized transcript snapshots, event catch-up and model catalog RPCs.
//! Called by the coordinator's Connect service; reads the agent sink, event log
//! and provider accounts without changing conversation state.

use connectrpc::ServiceResult;
use roost_proto as proto;

use super::rpc_errors::{agent_status, credential_status, internal};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

/// How many recently used models the catalog carries.
const RECENT_MODEL_LIMIT: i64 = 15;

pub async fn handle_agent_chat_snapshot(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatSnapshotRequest,
) -> ServiceResult<proto::AgentChatSnapshotResponse> {
    require_account_device(caller)?;
    if let Some(omp) = super::omp::rpc::find_omp(core, &request.conversation_id).await? {
        return super::omp::rpc::snapshot(core, omp).await;
    }
    let (seq, transcript) = core
        .services
        .agent
        .sink
        .snapshot(&request.conversation_id)
        .await
        .map_err(agent_status)?;
    let transcript_json =
        serde_json::to_string(&transcript).map_err(|_| internal("transcript did not serialize"))?;
    ok_response(proto::AgentChatSnapshotResponse {
        seq,
        transcript_json,
        ..Default::default()
    })
}

pub async fn handle_agent_chat_events(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatEventsRequest,
) -> ServiceResult<proto::AgentChatEventsResponse> {
    require_account_device(caller)?;
    let limit = if request.limit == 0 {
        500
    } else {
        request.limit.min(500)
    };
    let (rows, truncated) = super::chat_events::read_after(
        &core.services.db,
        &request.conversation_id,
        request.after_cursor,
        limit,
    )
    .await
    .map_err(|error| internal(&format!("chat event read failed: {error}")))?;
    ok_response(proto::AgentChatEventsResponse {
        batches: rows
            .into_iter()
            .map(|row| proto::AgentChatEventBatch {
                cursor: row.cursor,
                events_json: row.events_json,
                ..Default::default()
            })
            .collect(),
        truncated,
        ..Default::default()
    })
}

pub async fn handle_agent_models_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentModelsListRequest,
) -> ServiceResult<proto::AgentModelsListResponse> {
    require_account_device(caller)?;
    let mut catalog = super::rpc_accounts::models_catalog(&core.services.agent)
        .await
        .map_err(credential_status)?;
    catalog.recent_models =
        super::store::recent_models(&core.services.agent.sink.database(), RECENT_MODEL_LIMIT)
            .await
            .map_err(agent_status)?;
    let catalog_json =
        serde_json::to_string(&catalog).map_err(|_| internal("catalog did not serialize"))?;
    ok_response(proto::AgentModelsListResponse {
        catalog_json,
        ..Default::default()
    })
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentChatSnapshot",
        "agent::rpc_chat_transcript::handle_agent_chat_snapshot",
    ),
    (
        "AgentChatEvents",
        "agent::rpc_chat_transcript::handle_agent_chat_events",
    ),
    (
        "AgentModelsList",
        "agent::rpc_chat_transcript::handle_agent_models_list",
    ),
];
