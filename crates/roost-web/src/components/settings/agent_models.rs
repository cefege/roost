//! Settings → Agents → Models: credentials and model-provider sign-in.
//!
//! The catalog and credential mutations are UI-direct calls to the built-in
//! agent host through the coordinator; the provider list remains host-owned.

use std::collections::HashMap;

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{
    ListAgentModels, LogoutAgentProvider, SetAgentApiKey, StartAgentLogin,
};
use roost_protocol::wire::agent_chat::{CredentialKind, ModelsCatalog};

use crate::components::md::{Button, ButtonVariant, Card, Chip, EmptyState, TextField};
use crate::pump::{Pump, use_store};

use super::agent_login_dialog::AgentLoginDialog;

/// Configures provider credentials for Roost's built-in agent.
#[component]
pub fn AgentModelsPane() -> Element {
    let pump = use_store();
    let catalog = use_signal(|| None::<ModelsCatalog>);
    let error = use_signal(String::new);
    let mut api_keys = use_signal(HashMap::<String, String>::new);
    let mut login_id = use_signal(|| None::<String>);
    let mut refresh = use_signal(|| 0_u64);
    let enabled = pump
        .core()
        .borrow()
        .store()
        .coord_identity
        .as_ref()
        .is_some_and(|identity| identity.builtin_agent_enabled);

    let load_pump = pump.clone();
    use_effect(move || {
        let _revision = refresh();
        if enabled {
            load_catalog(load_pump.clone(), catalog, error);
        }
    });

    if !enabled {
        return rsx! {
            EmptyState {
                icon: "smart_toy",
                title: "Built-in agent unavailable",
                supporting: "The coordinator has not enabled the built-in agent host.",
            }
        };
    }

    let catalog_value = catalog();
    let providers = catalog_value
        .as_ref()
        .map(|value| value.providers.clone())
        .unwrap_or_default();
    let pump_for_login = pump.clone();
    rsx! {
        div { class: "settings-pane", "data-testid": "agent-models-pane",
            if !error().is_empty() {
                p { class: "md-body-s", role: "alert", {error()} }
            }
            if catalog_value.is_none() {
                Card { title: "Built-in agent models", supporting: "Loading provider settings…", }
            }
            for provider in providers {
                {
                    let provider_id = provider.id.clone();
                    let provider_id_for_key = provider.id.clone();
                    let provider_id_for_save = provider.id.clone();
                    let provider_id_for_logout = provider.id.clone();
                    let key_value = api_keys().get(&provider.id).cloned().unwrap_or_default();
                    let status = match provider.credential {
                        Some(CredentialKind::Oauth) => "Signed in",
                        Some(CredentialKind::ApiKey) => "API key",
                        Some(CredentialKind::Env) => "Env",
                        None => "Not configured",
                    };
                    let login_pump = pump_for_login.clone();
                    let login_error = error;
                    let save_pump = pump.clone();
                    let logout_pump = pump.clone();
                    rsx! {
                        Card {
                            key: "{provider_id}",
                            title: provider.name.clone(),
                            supporting: provider.id.clone(),
                            trailing: rsx! { Chip { label: status.to_owned(), icon: None } },
                            if provider.supports_oauth {
                                Button {
                                    variant: ButtonVariant::Outline,
                                    icon: "login",
                                    disabled: login_id().is_some(),
                                    onclick: move |_| start_login(login_pump.clone(), provider_id.clone(), login_id, login_error),
                                    "Sign in"
                                }
                            }
                            div { style: "display: flex; align-items: flex-end; gap: var(--md-space-3); margin-block-start: var(--md-space-3);",
                                TextField {
                                    label: "API key",
                                    input_type: "password",
                                    autocomplete: "new-password",
                                    value: key_value,
                                    on_input: move |value| {
                                        api_keys.write().insert(provider_id_for_key.clone(), value);
                                    },
                                }
                                Button {
                                    variant: ButtonVariant::Secondary,
                                    disabled: api_keys().get(&provider.id).is_none_or(|key| key.trim().is_empty()),
                                    onclick: move |_| save_api_key(save_pump.clone(), provider_id_for_save.clone(), api_keys, error, refresh),
                                    "Save"
                                }
                            }
                            if provider.configured {
                                Button {
                                    variant: ButtonVariant::Link,
                                    onclick: move |_| logout_provider(logout_pump.clone(), provider_id_for_logout.clone(), error, refresh),
                                    "Sign out"
                                }
                            }
                        }
                    }
                }
            }
            if let Some(id) = login_id() {
                AgentLoginDialog {
                    login_id: id,
                    on_close: move |completed| {
                        login_id.set(None);
                        if completed { refresh.set(refresh().wrapping_add(1)); }
                    },
                }
            }
        }
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

fn start_login(
    pump: Pump,
    provider: String,
    login_id: Signal<Option<String>>,
    error: Signal<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut login_id = login_id;
        let mut error = error;
        match pump.rpc().call(&StartAgentLogin { provider }).await {
            Ok(id) => {
                login_id.set(Some(id));
                error.set(String::new());
            }
            Err(failure) => error.set(format!("Could not start sign-in: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, provider, login_id, error);
}

fn save_api_key(
    pump: Pump,
    provider: String,
    keys: Signal<HashMap<String, String>>,
    error: Signal<String>,
    refresh: Signal<u64>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let api_key = keys().get(&provider).cloned().unwrap_or_default();
        let mut error = error;
        let mut refresh = refresh;
        match pump.rpc().call(&SetAgentApiKey { provider, api_key }).await {
            Ok(()) => {
                error.set(String::new());
                refresh.set(refresh().wrapping_add(1));
            }
            Err(failure) => error.set(format!("Could not save API key: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, provider, keys, error, refresh);
}

fn logout_provider(pump: Pump, provider: String, error: Signal<String>, refresh: Signal<u64>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        let mut refresh = refresh;
        match pump.rpc().call(&LogoutAgentProvider { provider }).await {
            Ok(()) => {
                error.set(String::new());
                refresh.set(refresh().wrapping_add(1));
            }
            Err(failure) => error.set(format!("Could not sign out: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, provider, error, refresh);
}
