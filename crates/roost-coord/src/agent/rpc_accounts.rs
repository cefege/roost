//! Device-authorized RPCs for connected accounts, usage, harness settings,
//! and the model catalog the browser's pickers read.

use std::collections::BTreeMap;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_agent::{AgentSettings, Role};
use roost_llm::{CredentialKind, ModelKind, StoredCredential};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::{
    AccountBrief, AccountEntry, AccountKind, AccountUsage, AccountUsageWindow, AgentSettingsView,
    ModelEntry, ModelsCatalog, ProviderEntry,
};

use super::AgentService;
use super::rpc_errors::{agent_status, internal};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

const OAUTH_PROVIDERS: [&str; 2] = ["anthropic", "openai-codex"];

fn provider_name(id: &str) -> String {
    match id {
        "anthropic" => "Anthropic".into(),
        "openai-codex" => "OpenAI Codex".into(),
        "openrouter" => "OpenRouter".into(),
        "typesafe" => "TypeSafe".into(),
        other => other.into(),
    }
}

fn account_kind(credential: &StoredCredential) -> AccountKind {
    match credential.kind {
        CredentialKind::OAuth { .. } => AccountKind::Oauth,
        CredentialKind::ApiKey { .. } => AccountKind::ApiKey,
    }
}

/// The catalog with availability, accounts per provider, and thinking levels.
pub async fn models_catalog(agent: &AgentService) -> ModelsCatalog {
    let catalog = agent.runtime.catalog();
    let mut providers = Vec::new();
    let mut connected = Vec::new();
    for provider in catalog.providers() {
        let accounts: Vec<AccountBrief> = agent
            .pool()
            .store()
            .list(provider)
            .await
            .iter()
            .map(|credential| AccountBrief {
                credential_id: credential.id,
                label: credential.label.clone(),
                kind: account_kind(credential),
            })
            .collect();
        if !accounts.is_empty() {
            connected.push(provider.to_owned());
        }
        providers.push(ProviderEntry {
            id: provider.to_owned(),
            name: provider_name(provider),
            configured: !accounts.is_empty(),
            accounts,
            supports_oauth: OAUTH_PROVIDERS.contains(&provider),
        });
    }
    let models = catalog
        .models()
        .iter()
        .map(|model| ModelEntry {
            provider: model.provider.clone(),
            model_id: model.id.clone(),
            name: model.name.clone(),
            reasoning: model.reasoning,
            available: connected.contains(&model.provider),
            classifier: model.kind == ModelKind::Classifier,
        })
        .collect();
    ModelsCatalog {
        models,
        providers,
        thinking_levels: agent.runtime.thinking_levels().await,
        default_model: agent.runtime.default_model().await,
    }
}

async fn account_entries(agent: &AgentService, with_usage: bool) -> Vec<AccountEntry> {
    let mut entries = Vec::new();
    let store = agent.pool().store();
    for provider in agent.runtime.catalog().providers() {
        let blocks: BTreeMap<i64, i64> = store.blocks(provider).await.into_iter().collect();
        for credential in store.list(provider).await {
            let report = if with_usage {
                agent.pool().refresh_usage(credential.id, provider).await
            } else {
                agent
                    .pool()
                    .usage_reports()
                    .into_iter()
                    .find(|report| report.credential_id == credential.id)
            };
            entries.push(AccountEntry {
                credential_id: credential.id,
                provider: provider.to_owned(),
                label: credential.label.clone(),
                kind: account_kind(&credential),
                disabled_cause: credential.disabled_cause.clone(),
                blocked_until_ms: blocks.get(&credential.id).copied(),
                usage: report.map(|report| AccountUsage {
                    windows: report
                        .windows
                        .into_iter()
                        .map(|window| AccountUsageWindow {
                            name: window.name,
                            used_fraction: window.used_fraction,
                            resets_at_ms: window.resets_at_ms,
                        })
                        .collect(),
                    note: report.note,
                    fetched_ms: report.fetched_ms,
                }),
            });
        }
    }
    entries
}

pub async fn handle_agent_accounts_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentAccountsListRequest,
) -> ServiceResult<proto::AgentAccountsListResponse> {
    require_account_device(caller)?;
    let entries = account_entries(&core.services.agent, false).await;
    let accounts_json =
        serde_json::to_string(&entries).map_err(|_| internal("accounts did not serialize"))?;
    ok_response(proto::AgentAccountsListResponse {
        accounts_json,
        ..Default::default()
    })
}

pub async fn handle_agent_account_remove(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentAccountRemoveRequest,
) -> ServiceResult<proto::AgentAccountRemoveResponse> {
    require_account_device(caller)?;
    core.services
        .agent
        .pool()
        .store()
        .delete(request.credential_id)
        .await;
    tracing::info!(
        credential_id = request.credential_id,
        "agent provider account removed"
    );
    ok_response(proto::AgentAccountRemoveResponse::default())
}

pub async fn handle_agent_usage_get(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentUsageGetRequest,
) -> ServiceResult<proto::AgentUsageGetResponse> {
    require_account_device(caller)?;
    let entries = account_entries(&core.services.agent, true).await;
    let usage_json =
        serde_json::to_string(&entries).map_err(|_| internal("usage did not serialize"))?;
    ok_response(proto::AgentUsageGetResponse {
        usage_json,
        ..Default::default()
    })
}

pub async fn handle_agent_settings_get(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentSettingsGetRequest,
) -> ServiceResult<proto::AgentSettingsGetResponse> {
    require_account_device(caller)?;
    let settings = core
        .services
        .agent
        .runtime
        .settings()
        .await
        .map_err(agent_status)?;
    let view = AgentSettingsView {
        model_roles: settings
            .model_roles
            .iter()
            .map(|(role, selector)| (role.as_str().to_owned(), selector.clone()))
            .collect(),
        default_model: settings.default_model,
        advisor_enabled: settings.advisor_enabled,
    };
    let settings_json =
        serde_json::to_string(&view).map_err(|_| internal("settings did not serialize"))?;
    ok_response(proto::AgentSettingsGetResponse {
        settings_json,
        ..Default::default()
    })
}

pub async fn handle_agent_settings_set(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentSettingsSetRequest,
) -> ServiceResult<proto::AgentSettingsSetResponse> {
    require_account_device(caller)?;
    let view: AgentSettingsView =
        serde_json::from_str(&request.settings_json).map_err(|error| {
            ConnectError::new(
                ErrorCode::InvalidArgument,
                format!("settings_json: {error}"),
            )
        })?;
    let mut model_roles = BTreeMap::new();
    for (name, selector) in view.model_roles {
        let role = Role::from_name(&name).ok_or_else(|| {
            ConnectError::new(ErrorCode::InvalidArgument, format!("unknown role {name}"))
        })?;
        if !selector.trim().is_empty() {
            model_roles.insert(role, selector);
        }
    }
    let settings = AgentSettings {
        model_roles,
        default_model: view.default_model,
        advisor_enabled: view.advisor_enabled,
    };
    core.services
        .agent
        .runtime
        .set_settings(settings)
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentSettingsSetResponse::default())
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentAccountsList",
        "agent::rpc_accounts::handle_agent_accounts_list",
    ),
    (
        "AgentAccountRemove",
        "agent::rpc_accounts::handle_agent_account_remove",
    ),
    (
        "AgentUsageGet",
        "agent::rpc_accounts::handle_agent_usage_get",
    ),
    (
        "AgentSettingsGet",
        "agent::rpc_accounts::handle_agent_settings_get",
    ),
    (
        "AgentSettingsSet",
        "agent::rpc_accounts::handle_agent_settings_set",
    ),
];
