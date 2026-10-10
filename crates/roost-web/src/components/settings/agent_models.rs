//! Settings → Agents → Models: compact provider credentials and connection.
//!
//! The catalog and credential mutations are UI-direct calls to the built-in
//! agent host through the coordinator; the provider list remains host-owned.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{ListAgentModels, RemoveAgentAccount};
use roost_protocol::wire::agent_chat::{AccountKind, ModelsCatalog, ProviderEntry};

use crate::components::md::{Button, ButtonVariant, Skeleton, TextField};
use crate::pump::{Pump, use_store};

use super::agent_connect_dialog::AgentConnectDialog;

/// Configures provider credentials for Roost's built-in agent.
#[component]
pub fn AgentModelsPane() -> Element {
    let pump = use_store();
    let catalog = use_signal(|| None::<ModelsCatalog>);
    let error = use_signal(String::new);
    let mut filter = use_signal(String::new);
    let mut connect_provider = use_signal(|| None::<(ProviderEntry, bool)>);
    let mut refresh = use_signal(|| 0_u64);
    let load_pump = pump.clone();
    use_effect(move || {
        let _revision = refresh();
        load_catalog(load_pump.clone(), catalog, error);
    });

    let catalog_value = catalog();
    let providers = catalog_value
        .as_ref()
        .map(|value| value.providers.clone())
        .unwrap_or_default();
    let needle = filter().trim().to_lowercase();
    let mut connected: Vec<_> = providers
        .iter()
        .filter(|provider| provider.configured)
        .cloned()
        .collect();
    connected.sort_by_key(|provider| provider.name.to_lowercase());
    let mut available: Vec<_> = providers
        .iter()
        .filter(|provider| !provider.configured)
        .filter(|provider| {
            needle.is_empty()
                || provider.name.to_lowercase().contains(&needle)
                || provider.id.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect();
    available.sort_by_key(|provider| provider.name.to_lowercase());

    rsx! {
        div { class: "settings-pane agent-settings", "data-testid": "agent-models-pane",
            if !error().is_empty() {
                p { class: "agent-settings__notice md-body-s", role: "alert", {error()} }
            }
            if catalog_value.is_none() {
                div { class: "agent-settings__section",
                    h2 { class: "agent-settings__section-title", "Connected" }
                    for index in 0..3 {
                        div { class: "agent-settings__skeleton-row", key: "connected-{index}",
                            Skeleton { width: Some("45%".to_owned()), class: None }
                            Skeleton { width: Some("24%".to_owned()), class: None }
                        }
                    }
                    h2 { class: "agent-settings__section-title", "Available providers" }
                    for index in 0..5 {
                        div { class: "agent-settings__skeleton-row", key: "available-{index}",
                            Skeleton { width: Some("58%".to_owned()), class: None }
                        }
                    }
                }
            } else {
                if !connected.is_empty() {
                    div { class: "agent-settings__section", "data-testid": "agent-connected-providers",
                        h2 { class: "agent-settings__section-title", "Connected" }
                        div { class: "agent-settings__provider-list",
                            for provider in connected {
                                {connected_provider_row(provider, pump.clone(), error, refresh, connect_provider)}
                            }
                        }
                    }
                }
                div { class: "agent-settings__section", "data-testid": "agent-available-providers",
                    h2 { class: "agent-settings__section-title", "Available providers" }
                    TextField {
                        value: filter(),
                        on_input: move |value| filter.set(value),
                        placeholder: Some("Search providers".to_owned()),
                        aria_label: Some("Search providers".to_owned()),
                        class: Some("agent-settings__filter".to_owned()),
                        input_type: Some("search".to_owned()),
                    }
                    if available.is_empty() {
                        p { class: "md-body-s", "No providers match this filter." }
                    } else {
                        div { class: "agent-settings__provider-list",
                            for provider in available {
                                div { class: "agent-settings__provider-row", key: "{provider.id}",
                                    div { class: "agent-settings__provider-info",
                                        span { class: "agent-settings__provider-name", "{provider.name}" }
                                        span { class: "agent-settings__provider-id", "{provider.id}" }
                                    }
                                    Button {
                                        variant: ButtonVariant::Outline,
                                        icon: "add",
                                        onclick: move |_| connect_provider.set(Some((provider.clone(), false))),
                                        "Connect"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some((provider, change_key)) = connect_provider() {
                AgentConnectDialog {
                    provider,
                    change_key,
                    on_close: move |completed| {
                        connect_provider.set(None);
                        if completed { refresh.set(refresh().wrapping_add(1)); }
                    },
                }
            }
        }
    }
}

fn connected_provider_row(
    provider: ProviderEntry,
    pump: Pump,
    error: Signal<String>,
    refresh: Signal<u64>,
    mut connect_provider: Signal<Option<(ProviderEntry, bool)>>,
) -> Element {
    let is_api_key = provider
        .accounts
        .iter()
        .any(|account| account.kind == AccountKind::ApiKey);
    let first_account = provider.accounts.first().cloned();
    let logout_pump = pump;
    rsx! {
        div { class: "agent-settings__provider-row", key: "{provider.id}",
            div { class: "agent-settings__provider-info",
                span { class: "agent-settings__provider-name", "{provider.name}" }
                span { class: "agent-settings__provider-id", "{provider.id}" }
                for account in provider.accounts.iter() {
                    span { class: "agent-settings__provider-id",
                        "{account.label} · {account_kind_label(account.kind)}"
                    }
                }
            }
            div { class: "agent-settings__provider-actions",
                if is_api_key {
                    Button {
                        variant: ButtonVariant::Outline,
                        onclick: move |_| connect_provider.set(Some((provider.clone(), true))),
                        "Change key"
                    }
                }
                if let Some(account) = first_account {
                    Button {
                        variant: ButtonVariant::Outline,
                        onclick: move |_| logout_account(logout_pump.clone(), account.credential_id, error, refresh),
                        "Sign out"
                    }
                }
            }
        }
    }
}

fn account_kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Oauth => "OAuth",
        AccountKind::ApiKey => "API key",
    }
}

fn load_catalog(pump: Pump, catalog: Signal<Option<ModelsCatalog>>, error: Signal<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut catalog = catalog;
        let mut error = error;
        match pump.rpc().call(&ListAgentModels).await {
            Ok(value) => {
                catalog.set(Some(value));
                error.set(String::new());
            }
            Err(failure) => error.set(format!("Could not load agent models: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, catalog, error);
}
fn logout_account(pump: Pump, credential_id: i64, error: Signal<String>, refresh: Signal<u64>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        let mut refresh = refresh;
        match pump.rpc().call(&RemoveAgentAccount { credential_id }).await {
            Ok(()) => {
                error.set(String::new());
                refresh.set(refresh().wrapping_add(1));
            }
            Err(failure) => error.set(format!("Could not sign out: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, credential_id, error, refresh);
}
