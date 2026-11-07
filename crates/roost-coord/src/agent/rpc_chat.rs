//! Device-authorized agent chat RPCs: conversation list, create, submit,
//! abort, configure, delete and plan decisions. Every conversation is a
//! worker-hosted OMP session; the native runtime is gone.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::{ModelRef, conversation_to_proto};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn handle_agent_chat_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentChatListRequest,
) -> ServiceResult<proto::AgentChatListResponse> {
    require_account_device(caller)?;
    let scopes = super::omp::store::settings(&core.services.db)
        .await
        .map_err(super::omp::rpc_settings::database_error)?;
    let mut conversations: Vec<_> = super::omp::store::list(&core.services.db)
        .await
        .map_err(super::omp::rpc_settings::database_error)?
        .iter()
        .map(|conversation| super::omp::summary::summary(conversation, &scopes))
        .collect();
    conversations.sort_by(|left, right| {
        right
            .updated_ms
            .cmp(&left.updated_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
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
        .map_err(|_| ConnectError::new(ErrorCode::Internal, "worker lookup failed"))?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "worker not found"))?;
    Ok((worker.fp, worker.label, worker.os))
}

fn conversation_not_found() -> ConnectError {
    ConnectError::new(ErrorCode::NotFound, "conversation not found")
}

/// What a configure or create request asks for the session model: leave it,
/// reset it to the inherited folder/global default, or pin an explicit selection.
pub(super) enum ModelUpdate {
    Unchanged,
    Clear,
    Set(ModelRef),
}

pub(super) fn model_update(
    provider: Option<&str>,
    model_id: Option<&str>,
) -> Result<ModelUpdate, ConnectError> {
    match (provider, model_id) {
        (None, None) => Ok(ModelUpdate::Unchanged),
        (Some(""), Some("")) => Ok(ModelUpdate::Clear),
        (Some(provider), Some(model_id)) if !provider.is_empty() && !model_id.is_empty() => {
            Ok(ModelUpdate::Set(ModelRef {
                provider: provider.to_owned(),
                model_id: model_id.to_owned(),
            }))
        }
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
    if request.backend.is_empty() || request.backend == "omp" {
        let worker = live_worker(core, &request.worker_fp).await?;
        return super::omp::rpc::create(core, request, worker).await;
    }
    Err(ConnectError::new(
        ErrorCode::InvalidArgument,
        format!("unsupported agent backend: {}", request.backend),
    ))
}

pub async fn handle_agent_chat_submit(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatSubmitRequest,
) -> ServiceResult<proto::AgentChatSubmitResponse> {
    require_account_device(caller)?;
    require_request_id(&request.request_id)?;
    let Some(omp) = super::omp::rpc::find_omp(core, &request.conversation_id).await? else {
        return Err(conversation_not_found());
    };
    super::omp::rpc::submit(core, omp, request).await
}

pub async fn handle_agent_chat_withdraw(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatWithdrawRequest,
) -> ServiceResult<proto::AgentChatWithdrawResponse> {
    require_account_device(caller)?;
    require_request_id(&request.request_id)?;
    if super::omp::rpc::find_omp(core, &request.conversation_id)
        .await?
        .is_none()
    {
        return Err(conversation_not_found());
    }
    ok_response(proto::AgentChatWithdrawResponse {
        withdrawn: false,
        ..Default::default()
    })
}

pub async fn handle_agent_chat_abort(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatAbortRequest,
) -> ServiceResult<proto::AgentChatAbortResponse> {
    require_account_device(caller)?;
    require_request_id(&request.request_id)?;
    let Some(omp) = super::omp::rpc::find_omp(core, &request.conversation_id).await? else {
        return Err(conversation_not_found());
    };
    super::omp::rpc::abort(core, omp, request).await
}

/// Every agent command carries a client-minted id so a retry is applied once.
pub(super) fn require_request_id(request_id: &str) -> Result<(), ConnectError> {
    if request_id.trim().is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "request_id is required",
        ));
    }
    Ok(())
}

pub async fn handle_agent_chat_configure(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatConfigureRequest,
) -> ServiceResult<proto::AgentConversation> {
    require_account_device(caller)?;
    let Some(omp) = super::omp::rpc::find_omp(core, &request.conversation_id).await? else {
        return Err(conversation_not_found());
    };
    super::omp::rpc::configure(core, omp, request).await
}

pub async fn handle_agent_chat_delete(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatDeleteRequest,
) -> ServiceResult<proto::AgentChatDeleteResponse> {
    require_account_device(caller)?;
    let Some(omp) = super::omp::rpc::find_omp(core, &request.conversation_id).await? else {
        return Err(conversation_not_found());
    };
    super::omp::rpc::delete(core, omp).await
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
];
