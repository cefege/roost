//! Models, provider accounts and web search in separate focused settings sections.
//! Mounted by `settings::pane`; loads the shared catalog, account usage and
//! settings document once, then gives each section its data and mutations.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AccountEntry, AgentSettingsView, ModelEntry, ModelsCatalog, ProviderEntry,
};

use super::agent_accounts::{ConnectedProviderCard, plural};
use super::agent_connect_dialog::AgentConnectDialog;
use super::agent_provider_list::AvailableProviders;
use super::agent_provider_status::{ProviderSummary, provider_summaries};
use super::agent_roles::{AgentRoles, load_settings, save_settings};
use crate::components::agent_chat::agent_alert::ERROR_TEXT_CLASS;
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, EmptyState, IconButton, IconButtonSize, Skeleton, TextField,
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
    let mut section = use_signal(|| "models");
    let settings = use_signal(AgentSettingsView::default);
    let mut search_url = use_signal(String::new);
    let saved_search_url = use_memo(move || settings().searxng_url.unwrap_or_default());
    use_effect(move || search_url.set(saved_search_url()));
    let load_pump = pump.clone();
    use_effect(move || {
        let _revision = refresh();
        load_everything(load_pump.clone(), catalog, accounts, error);
    });
    let settings_pump = pump.clone();
    use_effect(move || load_settings(settings_pump.clone(), settings, error));
    let now_ms = crate::platform::clock::WallClock.now_ms();
    let mut reload = move |()| refresh += 1;
    let Some(catalog_value) = catalog() else {
        return rsx! {
            div { class: "settings-pane agent-settings", "data-testid": "agent-models-pane",
                if !error().is_empty() { p { class: ERROR_TEXT_CLASS, role: "alert", {error()} } }
                Skeleton { class: "agent-settings__skeleton-tile" }
            }
        };
    };
    let account_rows = accounts();
    let (signed_in, not_signed_in): (Vec<ProviderSummary>, Vec<ProviderSummary>) =
        provider_summaries(&catalog_value, &account_rows, "model")
            .into_iter()
            .partition(|summary| summary.provider.configured);
    let web_search: Vec<ProviderSummary> =
        provider_summaries(&catalog_value, &account_rows, "web_search")
            .into_iter()
            .filter(|summary| KEYED_ENGINES.contains(&summary.provider.id.as_str()))
            .collect();
    rsx! {
        div { class: "settings-pane agent-settings", "data-testid": "agent-models-pane",
            if !error().is_empty() { p { class: ERROR_TEXT_CLASS, role: "alert", {error()} } }
            nav { class: "settings-tabs", role: "tablist", "aria-label": "Model settings",
                for (key, label, icon) in [("models", "Models", "smart_toy"), ("accounts", "Provider accounts", "key"), ("search", "Web search", "travel_explore")] {
                    Button {
                        variant: if section() == key { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                        size: ButtonSize::Sm,
                        icon,
                        id: format!("agent-settings-tab-{key}"),
                        role: "tab",
                        "aria-selected": (section() == key).to_string(),
                        "aria-controls": "agent-settings-panel",
                        tabindex: if section() == key { "0" } else { "-1" },
                        "data-testid": format!("agent-settings-tab-{key}"),
                        onclick: move |_| section.set(key),
                        onkeydown: move |event| select_section(event, section),
                        "{label}"
                    }
                }
            }
            div {
                id: "agent-settings-panel",
                class: "settings-tabpanel",
                role: "tabpanel",
                "aria-labelledby": format!("agent-settings-tab-{}", section()),
                if section() == "models" {
                    section { class: "agent-settings__section", "data-testid": "agent-model-roles",
                        p { class: "agent-settings__support",
                            "Choose a default once. Override a task or folder only when you need to. The chat picker changes one session."
                        }
                        if signed_in.is_empty() {
                            Button {
                                variant: ButtonVariant::Outline,
                                size: ButtonSize::Sm,
                                icon: "add_link",
                                onclick: move |_| section.set("accounts"),
                                "Connect a model provider"
                            }
                        }
                        AgentRoles { catalog: catalog_value.clone(), settings, error }
                    }
                } else if section() == "accounts" {
                    section { class: "agent-settings__section", "data-testid": "agent-connected-providers",
                        div { class: "agent-settings__section-head",
                            h2 { class: "agent-settings__section-title", "Connected providers" }
                            span { class: "agent-settings__count", {plural(signed_in.len(), "provider", "providers")} }
                            IconButton { icon: "refresh", label: "Refresh accounts and usage", title: "Refresh accounts and usage", size: IconButtonSize::IconSm, class: "agent-settings__section-action", onclick: move |_| reload(()) }
                        }
                        if signed_in.is_empty() {
                            EmptyState {
                                icon: "link_off",
                                title: "Connect your first model provider",
                                supporting: "Use an existing subscription or add an API key. Then choose a model in the Models section.",
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
                            h2 { class: "agent-settings__section-title", "Add a provider" }
                        }
                        AvailableProviders { providers: not_signed_in, on_connect: move |provider| connect_provider.set(Some(provider)) }
                    }
                } else {
                    section { class: "agent-settings__section", "data-testid": "agent-web-search",
                        h2 { class: "agent-settings__section-title", "Search services" }
                        p { class: "agent-settings__support", "Keyless engines work out of the box. Connect a service below to use its search API; engine order lives under Models → Customize by task." }
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
                        form { class: "settings-search-save",
                            onsubmit: move |event| {
                                event.prevent_default();
                                let mut updated = settings();
                                let value = search_url().trim().to_owned();
                                updated.searxng_url = (!value.is_empty()).then_some(value);
                                save_settings(pump.clone(), updated, settings, error);
                            },
                            TextField {
                                value: search_url(),
                                on_input: move |value| search_url.set(value),
                                label: "SearXNG server",
                                placeholder: "https://search.example.com",
                                test_id: "agent-searxng-url",
                                description: rsx! { "Leave empty to use the default search services." },
                            }
                            Button {
                                variant: ButtonVariant::Outline,
                                r#type: "submit",
                                disabled: search_url().trim() == saved_search_url(),
                                "Save server"
                            }
                        }
                    }
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

fn select_section(event: KeyboardEvent, mut section: Signal<&'static str>) {
    let keys = ["models", "accounts", "search"];
    let active = keys.iter().position(|key| *key == section()).unwrap_or(0);
    let next = match event.key().to_string().as_str() {
        "ArrowRight" => (active + 1) % keys.len(),
        "ArrowLeft" => (active + keys.len() - 1) % keys.len(),
        "Home" => 0,
        "End" => keys.len() - 1,
        _ => return,
    };
    event.prevent_default();
    section.set(keys[next]);
    crate::components::deck::deck_dom::focus_by_id(&format!("agent-settings-tab-{}", keys[next]));
}
