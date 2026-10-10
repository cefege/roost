//! Settings → Agents → Models: at a glance, which providers are signed in,
//! how many accounts each has and how close they are to their limits; then
//! the providers still to connect, the model roles and web search. Mounted by
//! `settings::pane`; loads the catalog, accounts with usage, and settings once
//! and hands each section what it draws.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AccountEntry, AgentSettingsView, ModelEntry, ModelsCatalog, ProviderEntry,
};

use super::agent_accounts::{ConnectedProviderCard, plural};
use super::agent_connect_dialog::AgentConnectDialog;
use super::agent_provider_list::AvailableProviders;
use super::agent_provider_status::{ProviderSummary, models_overview, provider_summaries};
use super::agent_roles::{AgentRoles, load_settings, save_settings};
use crate::components::md::{
    Button, ButtonVariant, EmptyState, IconButton, IconButtonSize, MetricTile, Skeleton, TextField,
};
use crate::pump::{Pump, use_store};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{
    GetAgentUsage, ListAgentAccounts, ListAgentModels,
};

/// Web-search engines that take a key; the keyless ones need no row here.
const KEYED_ENGINES: [&str; 8] = [
    "exa",
    "parallel",
    "firecrawl",
    "brave",
    "tavily",
    "perplexity",
    "jina",
    "kagi",
];

/// Configures provider credentials and model roles for Roost's built-in agent.
#[component]
pub fn AgentModelsPane() -> Element {
    let pump = use_store();
    let catalog = use_signal(|| None::<ModelsCatalog>);
    let accounts = use_signal(Vec::<AccountEntry>::new);
    let error = use_signal(String::new);
    let mut connect_provider = use_signal(|| None::<ProviderEntry>);
    let mut refresh = use_signal(|| 0_u64);
    let settings = use_signal(AgentSettingsView::default);
    let load_pump = pump.clone();
    use_effect(move || {
        let _revision = refresh();
        load_everything(load_pump.clone(), catalog, accounts, error);
    });
    let settings_pump = pump.clone();
    use_effect(move || load_settings(settings_pump.clone(), settings, error));
    // Resets and rate-limit ends are Unix times; the core clock is monotonic.
    let now_ms = crate::platform::clock::WallClock.now_ms();
    let mut reload = move |()| refresh += 1;
    let Some(catalog_value) = catalog() else {
        return rsx! {
            div { class: "settings-pane agent-settings", "data-testid": "agent-models-pane",
                if !error().is_empty() { p { class: "agent-settings__notice md-body-s", role: "alert", {error()} } }
                div { class: "agent-settings__overview",
                    for index in 0..4 { Skeleton { key: "tile-{index}", class: Some("agent-settings__skeleton-tile".to_owned()) } }
                }
            }
        };
    };
    let account_rows = accounts();
    let model_providers = provider_summaries(&catalog_value, &account_rows, "model");
    let overview = models_overview(&catalog_value, &model_providers, now_ms);
    let (signed_in, not_signed_in): (Vec<ProviderSummary>, Vec<ProviderSummary>) = model_providers
        .into_iter()
        .partition(|summary| summary.provider.configured);
    let web_search: Vec<ProviderSummary> =
        provider_summaries(&catalog_value, &account_rows, "web_search")
            .into_iter()
            .filter(|summary| KEYED_ENGINES.contains(&summary.provider.id.as_str()))
            .collect();
    let attention_support = if overview.accounts_needing_attention == 0 {
        "Every account is ready".to_owned()
    } else {
        "Rate-limited, disabled or near a limit".to_owned()
    };
    rsx! {
        div { class: "settings-pane agent-settings", "data-testid": "agent-models-pane",
            if !error().is_empty() { p { class: "agent-settings__notice md-body-s", role: "alert", {error()} } }
            div { class: "agent-settings__overview", "data-testid": "agent-models-overview",
                MetricTile {
                    label: "Signed in".to_owned(), icon: Some("verified_user".to_owned()),
                    value: format!("{} of {}", overview.signed_in_providers, overview.total_providers),
                    support: Some("model providers".to_owned()),
                }
                MetricTile {
                    label: "Accounts".to_owned(), icon: Some("group".to_owned()),
                    value: (overview.oauth_accounts + overview.api_key_accounts).to_string(),
                    support: Some(format!("{} subscription · {} API key", overview.oauth_accounts, overview.api_key_accounts)),
                }
                MetricTile {
                    label: "Models ready".to_owned(), icon: Some("smart_toy".to_owned()),
                    value: overview.ready_models.to_string(),
                    support: Some(format!("of {} known", overview.total_models)),
                }
                MetricTile {
                    label: "Needs attention".to_owned(),
                    icon: Some(if overview.accounts_needing_attention == 0 { "check_circle" } else { "warning" }.to_owned()),
                    value: overview.accounts_needing_attention.to_string(),
                    support: Some(attention_support),
                }
            }
            section { class: "agent-settings__section", "data-testid": "agent-connected-providers",
                div { class: "agent-settings__section-head",
                    h2 { class: "agent-settings__section-title", "Signed in" }
                    span { class: "agent-settings__muted md-label-m", {plural(signed_in.len(), "provider", "providers")} }
                    IconButton { icon: "refresh", label: "Refresh accounts and usage", title: "Refresh", size: IconButtonSize::IconSm, class: "agent-settings__section-action", onclick: move |_| reload(()) }
                }
                if signed_in.is_empty() {
                    EmptyState {
                        icon: "link_off".to_owned(),
                        title: "No model provider is signed in yet".to_owned(),
                        supporting: Some("The agent can't answer until one is. Sign in with a subscription you already pay for, or paste an API key.".to_owned()),
                        action: quick_sign_in(&not_signed_in, connect_provider),
                    }
                } else {
                    div { class: "agent-settings__provider-cards",
                        for summary in signed_in {
                            ConnectedProviderCard {
                                key: "{summary.provider.id}",
                                models: provider_models(&catalog_value, &summary.provider.id),
                                on_add: {
                                    let provider = summary.provider.clone();
                                    move |()| connect_provider.set(Some(provider.clone()))
                                },
                                summary,
                                now_ms,
                                on_changed: reload,
                            }
                        }
                    }
                }
            }
            section { class: "agent-settings__section", "data-testid": "agent-available-providers",
                div { class: "agent-settings__section-head",
                    h2 { class: "agent-settings__section-title", "Not signed in" }
                    span { class: "agent-settings__muted md-label-m", {plural(not_signed_in.len(), "provider", "providers")} }
                }
                AvailableProviders { providers: not_signed_in, on_connect: move |provider| connect_provider.set(Some(provider)) }
            }
            section { class: "agent-settings__section", "data-testid": "agent-model-roles",
                div { class: "agent-settings__section-head",
                    h2 { class: "agent-settings__section-title", "Model roles" }
                }
                p { class: "agent-settings__muted md-body-s", "Which model each kind of work uses. Leave a role on its default unless you want to pin it." }
                AgentRoles { catalog: catalog_value.clone(), settings, error }
            }
            section { class: "agent-settings__section", "data-testid": "agent-web-search",
                div { class: "agent-settings__section-head",
                    h2 { class: "agent-settings__section-title", "Web search" }
                }
                p { class: "agent-settings__muted md-body-s", "Keyless engines work out of the box; a key unlocks the ones below." }
                for summary in web_search.iter().filter(|summary| summary.provider.configured).cloned() {
                    ConnectedProviderCard {
                        key: "{summary.provider.id}",
                        models: Vec::new(),
                        on_add: {
                            let provider = summary.provider.clone();
                            move |()| connect_provider.set(Some(provider.clone()))
                        },
                        summary,
                        now_ms,
                        on_changed: reload,
                    }
                }
                AvailableProviders {
                    providers: web_search.iter().filter(|summary| !summary.provider.configured).cloned().collect::<Vec<_>>(),
                    on_connect: move |provider| connect_provider.set(Some(provider)),
                }
                TextField {
                    value: settings().searxng_url.clone().unwrap_or_default(),
                    on_input: move |value: String| {
                        let mut updated = settings();
                        updated.searxng_url = if value.trim().is_empty() { None } else { Some(value) };
                        save_settings(pump.clone(), updated, settings, error);
                    },
                    label: Some("SearXNG URL".to_owned()),
                    placeholder: Some("https://search.example.com".to_owned()),
                    aria_label: Some("SearXNG URL".to_owned()),
                    test_id: Some("agent-searxng-url".to_owned()),
                }
            }
            if let Some(provider) = connect_provider() {
                AgentConnectDialog { provider, on_close: move |_| { connect_provider.set(None); reload(()); } }
            }
        }
    }
}

/// The first-run action: one button per provider that signs in with a
/// subscription, since that needs no key to hand.
fn quick_sign_in(
    not_signed_in: &[ProviderSummary],
    mut connect_provider: Signal<Option<ProviderEntry>>,
) -> Option<Element> {
    let oauth: Vec<ProviderEntry> = not_signed_in
        .iter()
        .filter(|summary| summary.provider.supports_oauth)
        .map(|summary| summary.provider.clone())
        .collect();
    if oauth.is_empty() {
        return None;
    }
    Some(rsx! {
        div { class: "agent-settings__quick-sign-in",
            for provider in oauth {
                Button {
                    key: "{provider.id}",
                    variant: ButtonVariant::Default,
                    icon: "login",
                    onclick: {
                        let provider = provider.clone();
                        move |_| connect_provider.set(Some(provider.clone()))
                    },
                    "Sign in with {provider.name}"
                }
            }
        }
    })
}

/// A provider's chat models, in the catalog's newest-first order.
fn provider_models(catalog: &ModelsCatalog, provider: &str) -> Vec<ModelEntry> {
    catalog
        .models
        .iter()
        .filter(|model| model.provider == provider && !model.classifier)
        .cloned()
        .collect()
}

/// The catalog first, then accounts, then usage: usage asks every provider
/// for its limits and is the slow call, so the rows draw before it lands.
fn load_everything(
    pump: Pump,
    catalog: Signal<Option<ModelsCatalog>>,
    accounts: Signal<Vec<AccountEntry>>,
    error: Signal<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let (mut catalog, mut accounts, mut error) = (catalog, accounts, error);
        match pump.rpc().call(&ListAgentModels).await {
            Ok(value) => {
                catalog.set(Some(value));
                error.set(String::new());
            }
            Err(failure) => {
                error.set(format!("Could not load the model list: {failure}"));
                return;
            }
        }
        match pump
            .rpc()
            .call(&ListAgentAccounts {})
            .await
            .map(|json| serde_json::from_str::<Vec<AccountEntry>>(&json))
        {
            Ok(Ok(rows)) => accounts.set(rows),
            Ok(Err(failure)) => error.set(format!("Could not read the account list: {failure}")),
            Err(failure) => error.set(format!("Could not load accounts: {failure}")),
        }
        let usage = match pump
            .rpc()
            .call(&GetAgentUsage {})
            .await
            .map(|json| serde_json::from_str::<Vec<AccountEntry>>(&json))
        {
            Ok(Ok(usage)) => usage,
            Ok(Err(failure)) => {
                tracing::warn!(target: "agent_settings", %failure, "agent usage unreadable");
                return;
            }
            Err(failure) => {
                tracing::warn!(target: "agent_settings", %failure, "agent usage unavailable");
                return;
            }
        };
        let mut rows = accounts();
        for row in &mut rows {
            if let Some(fresh) = usage
                .iter()
                .find(|item| item.credential_id == row.credential_id)
            {
                row.usage = fresh.usage.clone();
                row.blocked_until_ms = fresh.blocked_until_ms;
                row.disabled_cause = fresh.disabled_cause.clone();
            }
        }
        accounts.set(rows);
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, catalog, accounts, error);
}
