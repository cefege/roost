//! Device-authorized provider sign-in RPCs: OAuth login start/poll/respond/
//! cancel through the login registry, and API keys stored as accounts keyed
//! by their fingerprint. Secrets never reach a log line.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_llm::{CredentialKind, oauth::api_key_identity};
use roost_proto as proto;

use super::rpc_errors::{internal, login_status};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

/// Providers that accept an API key.
const API_KEY_PROVIDERS: [&str; 3] = ["anthropic", "openrouter", "typesafe"];

pub async fn handle_agent_auth_login_start(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthLoginStartRequest,
) -> ServiceResult<proto::AgentAuthLoginStartResponse> {
    require_account_device(caller)?;
    let agent = &core.services.agent;
    let login_id = agent
        .logins
        .start(
            &request.provider,
            agent.http.clone(),
            agent.endpoints.clone(),
            agent.pool().clone(),
        )
        .await
        .map_err(login_status)?;
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
        .agent
        .logins
        .state(&request.login_id)
        .map_err(login_status)?;
    let state_json =
        serde_json::to_string(&state).map_err(|_| internal("login state did not serialize"))?;
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
    let agent = &core.services.agent;
    agent
        .logins
        .respond(
            &request.login_id,
            &request.prompt_id,
            &request.value,
            agent.pool(),
        )
        .await
        .map_err(login_status)?;
    ok_response(proto::AgentAuthLoginRespondResponse::default())
}

pub async fn handle_agent_auth_login_cancel(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthLoginCancelRequest,
) -> ServiceResult<proto::AgentAuthLoginCancelResponse> {
    require_account_device(caller)?;
    core.services.agent.logins.cancel(&request.login_id);
    ok_response(proto::AgentAuthLoginCancelResponse::default())
}

pub async fn handle_agent_auth_set_api_key(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAuthSetApiKeyRequest,
) -> ServiceResult<proto::AgentAuthSetApiKeyResponse> {
    require_account_device(caller)?;
    if !API_KEY_PROVIDERS.contains(&request.provider.as_str()) {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            format!("{} does not take an API key", request.provider),
        ));
    }
    let key = request.api_key.trim();
    if key.is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "API key is empty",
        ));
    }
    let (identity_key, label) = api_key_identity(key);
    let credential_id = core
        .services
        .agent
        .pool()
        .store()
        .upsert(
            &request.provider,
            CredentialKind::ApiKey {
                key: key.to_owned(),
            },
            &identity_key,
            &label,
        )
        .await;
    tracing::info!(provider = %request.provider, credential_id, "agent provider API key stored");
    ok_response(proto::AgentAuthSetApiKeyResponse::default())
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentAuthLoginStart",
        "agent::rpc_auth::handle_agent_auth_login_start",
    ),
    (
        "AgentAuthLoginPoll",
        "agent::rpc_auth::handle_agent_auth_login_poll",
    ),
    (
        "AgentAuthLoginRespond",
        "agent::rpc_auth::handle_agent_auth_login_respond",
    ),
    (
        "AgentAuthLoginCancel",
        "agent::rpc_auth::handle_agent_auth_login_cancel",
    ),
    (
        "AgentAuthSetApiKey",
        "agent::rpc_auth::handle_agent_auth_set_api_key",
    ),
];
