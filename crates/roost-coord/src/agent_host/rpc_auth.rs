//! Device-authorized RPCs for the agent host's provider sign-in lifecycle.
//!
//! The host owns OAuth state and credentials; this module forwards only the
//! browser's prompt responses and never copies provider secrets into logs.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;

use crate::agent_host::to_connect;
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn handle_agent_auth_login_start(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthLoginStartRequest,
) -> ServiceResult<proto::AgentAuthLoginStartResponse> {
    require_account_device(caller)?;
    let response = core
        .services
        .agent_host
        .require_client(&core.services)?
        .login_start(&request.provider)
        .await
        .map_err(to_connect)?;
    let login_id = response
        .get("login_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(invalid_host_response)?
        .to_owned();
    ok_response(proto::AgentAuthLoginStartResponse {
        login_id,
        ..Default::default()
    })
}

pub async fn handle_agent_auth_login_poll(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthLoginPollRequest,
) -> ServiceResult<proto::AgentAuthLoginPollResponse> {
    require_account_device(caller)?;
    let state = core
        .services
        .agent_host
        .require_client(&core.services)?
        .login_poll(&request.login_id)
        .await
        .map_err(to_connect)?;
    let state_json = serde_json::to_string(&state).map_err(|_| invalid_host_response())?;
    ok_response(proto::AgentAuthLoginPollResponse {
        state_json,
        ..Default::default()
    })
}

pub async fn handle_agent_auth_login_respond(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthLoginRespondRequest,
) -> ServiceResult<proto::AgentAuthLoginRespondResponse> {
    require_account_device(caller)?;
    core.services
        .agent_host
        .require_client(&core.services)?
        .login_respond(&request.login_id, &request.prompt_id, &request.value)
        .await
        .map_err(to_connect)?;
    ok_response(proto::AgentAuthLoginRespondResponse::default())
}

pub async fn handle_agent_auth_login_cancel(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthLoginCancelRequest,
) -> ServiceResult<proto::AgentAuthLoginCancelResponse> {
    require_account_device(caller)?;
    core.services
        .agent_host
        .require_client(&core.services)?
        .login_cancel(&request.login_id)
        .await
        .map_err(to_connect)?;
    ok_response(proto::AgentAuthLoginCancelResponse::default())
}

pub async fn handle_agent_auth_set_api_key(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthSetApiKeyRequest,
) -> ServiceResult<proto::AgentAuthSetApiKeyResponse> {
    require_account_device(caller)?;
    core.services
        .agent_host
        .require_client(&core.services)?
        .set_api_key(&request.provider, &request.api_key)
        .await
        .map_err(to_connect)?;
    ok_response(proto::AgentAuthSetApiKeyResponse::default())
}

pub async fn handle_agent_auth_logout(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthLogoutRequest,
) -> ServiceResult<proto::AgentAuthLogoutResponse> {
    require_account_device(caller)?;
    core.services
        .agent_host
        .require_client(&core.services)?
        .logout(&request.provider)
        .await
        .map_err(to_connect)?;
    ok_response(proto::AgentAuthLogoutResponse::default())
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentAuthLoginStart",
        "agent_host::rpc_auth::handle_agent_auth_login_start",
    ),
    (
        "AgentAuthLoginPoll",
        "agent_host::rpc_auth::handle_agent_auth_login_poll",
    ),
    (
        "AgentAuthLoginRespond",
        "agent_host::rpc_auth::handle_agent_auth_login_respond",
    ),
    (
        "AgentAuthLoginCancel",
        "agent_host::rpc_auth::handle_agent_auth_login_cancel",
    ),
    (
        "AgentAuthSetApiKey",
        "agent_host::rpc_auth::handle_agent_auth_set_api_key",
    ),
    (
        "AgentAuthLogout",
        "agent_host::rpc_auth::handle_agent_auth_logout",
    ),
];

fn invalid_host_response() -> ConnectError {
    ConnectError::new(
        ErrorCode::Internal,
        "agent host returned an invalid response",
    )
}
