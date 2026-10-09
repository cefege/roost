//! Device-authorized coordinator RPCs for the built-in agent conversation host.
//!
//! The durable host owns mutations; the local cache serves list and transcript
//! snapshots populated by the host stream follower. Worker metadata is resolved
//! from the coordinator registry before it crosses the host boundary.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::conversation_to_proto;

use crate::agent_host::{AgentChatUpdate, to_connect};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn handle_agent_chat_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentChatListRequest,
) -> ServiceResult<proto::AgentChatListResponse> {
    require_account_device(caller)?;
    core.services.agent_host.require_client(&core.services)?;
    let cache = core
        .services
        .agent_host
        .cache
        .lock()
        .map_err(|_| internal_error())?;
    let (host_connected, conversations) = cache.conversations();
    ok_response(proto::AgentChatListResponse {
        conversations: conversations.iter().map(conversation_to_proto).collect(),
        host_connected,
        ..Default::default()
    })
}

pub async fn handle_agent_chat_create(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatCreateRequest,
) -> ServiceResult<proto::AgentConversation> {
    require_account_device(caller)?;
    let client = core.services.agent_host.require_client(&core.services)?;
    let worker = crate::workers::rows::read_live_worker(&core.services.db, &request.worker_fp)
        .await
        .map_err(|_| internal_error())?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "worker not found"))?;
    if request.model_provider.is_empty() != request.model_id.is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "model_provider and model_id must be set together",
        ));
    }
    let model = (!request.model_provider.is_empty()).then(
        || serde_json::json!({"provider": request.model_provider, "model_id": request.model_id}),
    );
    let body = serde_json::json!({
        "worker_fp": worker.fp,
        "worker_label": worker.label,
        "worker_os": worker.os,
        "cwd": request.cwd,
        "model": model,
    });
    let summary = client.create(&body).await.map_err(to_connect)?;
    ok_response(conversation_to_proto(&summary))
}

pub async fn handle_agent_chat_submit(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatSubmitRequest,
) -> ServiceResult<proto::AgentChatSubmitResponse> {
    require_account_device(caller)?;
    core.services
        .agent_host
        .require_client(&core.services)?
        .submit(&request.conversation_id, &request.text, &request.request_id)
        .await
        .map_err(to_connect)?;
    ok_response(proto::AgentChatSubmitResponse::default())
}

pub async fn handle_agent_chat_abort(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatAbortRequest,
) -> ServiceResult<proto::AgentChatAbortResponse> {
    require_account_device(caller)?;
    core.services
        .agent_host
        .require_client(&core.services)?
        .abort(&request.conversation_id)
        .await
        .map_err(to_connect)?;
    ok_response(proto::AgentChatAbortResponse::default())
}

pub async fn handle_agent_chat_configure(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatConfigureRequest,
) -> ServiceResult<proto::AgentConversation> {
    require_account_device(caller)?;
    let client = core.services.agent_host.require_client(&core.services)?;
    let mut body = serde_json::Map::new();
    if request.model_provider.is_some() != request.model_id.is_some() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "model_provider and model_id must be set together",
        ));
    }
    if let (Some(provider), Some(model_id)) = (&request.model_provider, &request.model_id) {
        body.insert(
            "model".into(),
            serde_json::json!({"provider": provider, "model_id": model_id}),
        );
    }
    if let Some(level) = request.thinking_level {
        body.insert("thinking_level".into(), level.into());
    }
    if let Some(fp) = request.worker_fp {
        let worker = crate::workers::rows::read_live_worker(&core.services.db, &fp)
            .await
            .map_err(|_| internal_error())?
            .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "worker not found"))?;
        body.insert("worker_fp".into(), worker.fp.into());
        body.insert("worker_label".into(), worker.label.into());
        body.insert("worker_os".into(), worker.os.into());
    }
    if let Some(cwd) = request.cwd {
        body.insert("cwd".into(), cwd.into());
    }
    if let Some(title) = request.title {
        body.insert("title".into(), title.into());
    }
    let summary = client
        .configure(&request.conversation_id, &body)
        .await
        .map_err(to_connect)?;
    ok_response(conversation_to_proto(&summary))
}

pub async fn handle_agent_chat_delete(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatDeleteRequest,
) -> ServiceResult<proto::AgentChatDeleteResponse> {
    require_account_device(caller)?;
    core.services
        .agent_host
        .require_client(&core.services)?
        .delete(&request.conversation_id)
        .await
        .map_err(to_connect)?;
    core.services.agent_host.publish(
        &core.services,
        AgentChatUpdate::ConversationRemoved {
            id: request.conversation_id,
        },
    );
    ok_response(proto::AgentChatDeleteResponse::default())
}

pub async fn handle_agent_chat_snapshot(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatSnapshotRequest,
) -> ServiceResult<proto::AgentChatSnapshotResponse> {
    require_account_device(caller)?;
    core.services.agent_host.require_client(&core.services)?;
    let cache = core
        .services
        .agent_host
        .cache
        .lock()
        .map_err(|_| internal_error())?;
    let (_, conversations) = cache.conversations();
    if !conversations
        .iter()
        .any(|conversation| conversation.id == request.conversation_id)
    {
        return Err(ConnectError::new(
            ErrorCode::NotFound,
            "agent conversation not found",
        ));
    }
    let (seq, transcript_json) = cache.snapshot(&request.conversation_id).ok_or_else(|| {
        ConnectError::new(
            ErrorCode::Unavailable,
            "agent transcript is not available yet",
        )
    })?;
    ok_response(proto::AgentChatSnapshotResponse {
        seq,
        transcript_json,
        ..Default::default()
    })
}

pub async fn handle_agent_models_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentModelsListRequest,
) -> ServiceResult<proto::AgentModelsListResponse> {
    require_account_device(caller)?;
    let catalog = core
        .services
        .agent_host
        .require_client(&core.services)?
        .models()
        .await
        .map_err(to_connect)?;
    let catalog_json = serde_json::to_string(&catalog).map_err(|_| internal_error())?;
    ok_response(proto::AgentModelsListResponse {
        catalog_json,
        ..Default::default()
    })
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentChatList",
        "agent_host::rpc_chat::handle_agent_chat_list",
    ),
    (
        "AgentChatCreate",
        "agent_host::rpc_chat::handle_agent_chat_create",
    ),
    (
        "AgentChatSubmit",
        "agent_host::rpc_chat::handle_agent_chat_submit",
    ),
    (
        "AgentChatAbort",
        "agent_host::rpc_chat::handle_agent_chat_abort",
    ),
    (
        "AgentChatConfigure",
        "agent_host::rpc_chat::handle_agent_chat_configure",
    ),
    (
        "AgentChatDelete",
        "agent_host::rpc_chat::handle_agent_chat_delete",
    ),
    (
        "AgentChatSnapshot",
        "agent_host::rpc_chat::handle_agent_chat_snapshot",
    ),
    (
        "AgentModelsList",
        "agent_host::rpc_chat::handle_agent_models_list",
    ),
];

fn internal_error() -> ConnectError {
    ConnectError::new(ErrorCode::Internal, "agent conversation request failed")
}
