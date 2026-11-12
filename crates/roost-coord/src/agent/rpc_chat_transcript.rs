//! Device-authorized transcript snapshots and event catch-up RPCs.
//! Called by the coordinator's Connect service; reads the event log without
//! changing conversation state.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn handle_agent_chat_snapshot(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatSnapshotRequest,
) -> ServiceResult<proto::AgentChatSnapshotResponse> {
    require_account_device(caller)?;
    if let Some(omp) = super::omp::rpc::find_omp(core, &request.conversation_id).await? {
        return super::omp::rpc::snapshot(core, omp).await;
    }
    Err(ConnectError::new(
        ErrorCode::NotFound,
        "conversation not found",
    ))
}

pub async fn handle_agent_chat_events(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatEventsRequest,
) -> ServiceResult<proto::AgentChatEventsResponse> {
    require_account_device(caller)?;
    if super::omp::rpc::find_omp(core, &request.conversation_id)
        .await?
        .is_none()
    {
        return Err(ConnectError::new(
            ErrorCode::NotFound,
            "conversation not found",
        ));
    }
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
    .map_err(|error| {
        tracing::error!(%error, "agent chat event read failed");
        ConnectError::new(ErrorCode::Internal, "chat event read failed")
    })?;
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

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentChatSnapshot",
        "agent::rpc_chat_transcript::handle_agent_chat_snapshot",
    ),
    (
        "AgentChatEvents",
        "agent::rpc_chat_transcript::handle_agent_chat_events",
    ),
];
