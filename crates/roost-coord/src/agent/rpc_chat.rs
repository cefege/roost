//! Device-authorized agent chat RPCs over the harness: conversation list,
//! create, submit, abort, configure, delete, transcript snapshot, the model
//! catalog, and plan decisions. Worker metadata comes from the registry.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_agent::NewConversation;
use roost_proto as proto;
use roost_protocol::wire::agent_chat::{ModelRef, conversation_to_proto};

use super::rpc_errors::{agent_status, internal};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn handle_agent_chat_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentChatListRequest,
) -> ServiceResult<proto::AgentChatListResponse> {
    require_account_device(caller)?;
    let conversations = core
        .services
        .agent
        .runtime
        .conversations()
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentChatListResponse {
        conversations: conversations.iter().map(conversation_to_proto).collect(),
        host_connected: true,
        ..Default::default()
    })
}

async fn live_worker(
    core: &CoordCore,
    worker_fp: &str,
) -> Result<(String, String, String), ConnectError> {
    let worker = crate::workers::rows::read_live_worker(&core.services.db, worker_fp)
        .await
        .map_err(|_| internal("worker lookup failed"))?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "worker not found"))?;
    Ok((worker.fp, worker.label, worker.os))
}

fn model_pair(
    provider: Option<&str>,
    model_id: Option<&str>,
) -> Result<Option<ModelRef>, ConnectError> {
    match (
        provider.filter(|value| !value.is_empty()),
        model_id.filter(|value| !value.is_empty()),
    ) {
        (Some(provider), Some(model_id)) => Ok(Some(ModelRef {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
        })),
        (None, None) => Ok(None),
        _ => Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "model_provider and model_id must be set together",
        )),
    }
}

pub async fn handle_agent_chat_create(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatCreateRequest,
) -> ServiceResult<proto::AgentConversation> {
    require_account_device(caller)?;
    let (worker_fp, worker_label, worker_os) = live_worker(core, &request.worker_fp).await?;
    let model = model_pair(Some(&request.model_provider), Some(&request.model_id))?;
    let summary = core
        .services
        .agent
        .runtime
        .create_conversation(NewConversation {
            worker_fp,
            worker_label,
            worker_os,
            cwd: request.cwd,
            title: None,
            model,
            thinking_level: None,
        })
        .await
        .map_err(agent_status)?;
    ok_response(conversation_to_proto(&summary))
}

pub async fn handle_agent_chat_submit(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatSubmitRequest,
) -> ServiceResult<proto::AgentChatSubmitResponse> {
    require_account_device(caller)?;
    core.services
        .agent
        .runtime
        .submit(&request.conversation_id, request.text)
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentChatSubmitResponse::default())
}

pub async fn handle_agent_chat_abort(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatAbortRequest,
) -> ServiceResult<proto::AgentChatAbortResponse> {
    require_account_device(caller)?;
    core.services.agent.runtime.abort(&request.conversation_id);
    ok_response(proto::AgentChatAbortResponse::default())
}

pub async fn handle_agent_chat_configure(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatConfigureRequest,
) -> ServiceResult<proto::AgentConversation> {
    require_account_device(caller)?;
    let runtime = &core.services.agent.runtime;
    let id = &request.conversation_id;
    if let Some(model) = model_pair(
        request.model_provider.as_deref(),
        request.model_id.as_deref(),
    )? {
        runtime.set_model(id, model).await.map_err(agent_status)?;
    }
    if let Some(level) = request.thinking_level {
        runtime
            .set_thinking_level(id, level)
            .await
            .map_err(agent_status)?;
    }
    let worker = match request.worker_fp.as_deref() {
        Some(fp) => Some(live_worker(core, fp).await?),
        None => None,
    };
    let summary = runtime
        .set_details(id, request.title, worker, request.cwd)
        .await
        .map_err(agent_status)?;
    ok_response(conversation_to_proto(&summary))
}

pub async fn handle_agent_chat_delete(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatDeleteRequest,
) -> ServiceResult<proto::AgentChatDeleteResponse> {
    require_account_device(caller)?;
    core.services
        .agent
        .runtime
        .delete_conversation(&request.conversation_id)
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentChatDeleteResponse::default())
}

pub async fn handle_agent_chat_snapshot(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatSnapshotRequest,
) -> ServiceResult<proto::AgentChatSnapshotResponse> {
    require_account_device(caller)?;
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

pub async fn handle_agent_models_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentModelsListRequest,
) -> ServiceResult<proto::AgentModelsListResponse> {
    require_account_device(caller)?;
    let catalog = super::rpc_accounts::models_catalog(&core.services.agent).await;
    let catalog_json =
        serde_json::to_string(&catalog).map_err(|_| internal("catalog did not serialize"))?;
    ok_response(proto::AgentModelsListResponse {
        catalog_json,
        ..Default::default()
    })
}

pub async fn handle_agent_chat_plan_decide(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatPlanDecideRequest,
) -> ServiceResult<proto::AgentChatPlanDecideResponse> {
    require_account_device(caller)?;
    let created = core
        .services
        .agent
        .runtime
        .plan_decide(
            &request.conversation_id,
            &request.item_id,
            &request.decision,
            &request.feedback,
        )
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentChatPlanDecideResponse {
        new_conversation_id: created.unwrap_or_default(),
        ..Default::default()
    })
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    ("AgentChatList", "agent::rpc_chat::handle_agent_chat_list"),
    (
        "AgentChatCreate",
        "agent::rpc_chat::handle_agent_chat_create",
    ),
    (
        "AgentChatSubmit",
        "agent::rpc_chat::handle_agent_chat_submit",
    ),
    ("AgentChatAbort", "agent::rpc_chat::handle_agent_chat_abort"),
    (
        "AgentChatConfigure",
        "agent::rpc_chat::handle_agent_chat_configure",
    ),
    (
        "AgentChatDelete",
        "agent::rpc_chat::handle_agent_chat_delete",
    ),
    (
        "AgentChatSnapshot",
        "agent::rpc_chat::handle_agent_chat_snapshot",
    ),
    (
        "AgentModelsList",
        "agent::rpc_chat::handle_agent_models_list",
    ),
    (
        "AgentChatPlanDecide",
        "agent::rpc_chat::handle_agent_chat_plan_decide",
    ),
];
