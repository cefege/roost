//! Settings → Agents → Models: provider connections, accounts and roles.

use super::agent_accounts::AgentAccounts;
use super::agent_connect_dialog::AgentConnectDialog;
use super::agent_roles::AgentRoles;
use crate::components::md::{Button, ButtonVariant, Skeleton, TextField};
use crate::pump::{Pump, use_store};
use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ListAgentModels;
use roost_protocol::wire::agent_chat::{ModelsCatalog, ProviderEntry};

/// Configures provider credentials and model roles for Roost's built-in agent.
#[component]
pub fn AgentModelsPane() -> Element {
    let pump = use_store();
    let catalog = use_signal(|| None::<ModelsCatalog>);
    let error = use_signal(String::new);
    let mut filter = use_signal(String::new);
    let mut connect_provider = use_signal(|| None::<ProviderEntry>);
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
            if !error().is_empty() { p { class: "agent-settings__notice md-body-s", role: "alert", {error()} } }
            if let Some(value) = catalog_value.clone() {
                div { class: "agent-settings__section", "data-testid": "agent-connected-providers",
                    h2 { class: "agent-settings__section-title", "Connected accounts" }
                    for provider in connected {
                        div { class: "agent-settings__provider-row", key: "{provider.id}",
                            div { class: "agent-settings__provider-info",
                                span { class: "agent-settings__provider-name", "{provider.name}" }
                                span { class: "agent-settings__provider-id", "{provider.id}" }
                            }
                            AgentAccounts { provider: provider.id.clone(), refresh: refresh(), on_add: move |_| connect_provider.set(Some(provider.clone())) }
                        }
                    }
                }
                div { class: "agent-settings__section", "data-testid": "agent-available-providers",
                    h2 { class: "agent-settings__section-title", "Available providers" }
                    TextField { value: filter(), on_input: move |value| filter.set(value), placeholder: Some("Search providers".to_owned()), aria_label: Some("Search providers".to_owned()), class: Some("agent-settings__filter".to_owned()), input_type: Some("search".to_owned()) }
                    if available.is_empty() { p { class: "md-body-s", "No providers match this filter." } }
                    else { div { class: "agent-settings__provider-list", for provider in available {
                        div { class: "agent-settings__provider-row", key: "{provider.id}",
                            div { class: "agent-settings__provider-info", span { class: "agent-settings__provider-name", "{provider.name}" }, span { class: "agent-settings__provider-id", "{provider.id}" } }
                            Button { variant: ButtonVariant::Outline, icon: "add", onclick: move |_| connect_provider.set(Some(provider.clone())), "Connect" }
                        }
                    } } }
                }
                AgentRoles { catalog: value }
            } else {
                div { class: "agent-settings__section", h2 { class: "agent-settings__section-title", "Connected" }
                    for index in 0..3 { div { class: "agent-settings__skeleton-row", key: "connected-{index}", Skeleton { width: Some("45%".to_owned()), class: None } } }
                }
            }
            if let Some(provider) = connect_provider() {
                AgentConnectDialog { provider, on_close: move |_| { connect_provider.set(None); refresh.set(refresh().wrapping_add(1)); } }
            }
        }
    }
}

fn load_catalog(pump: Pump, catalog: Signal<Option<ModelsCatalog>>, error: Signal<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        match pump.rpc().call(&ListAgentModels).await {
            Ok(value) => {
                let mut catalog = catalog;
                catalog.set(Some(value));
                error.set(String::new());
            }
            Err(failure) => error.set(format!("Could not load agent models: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, catalog, error);
}
