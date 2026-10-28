//! A focused credential flow for one agent model provider.
//! OAuth stays in the existing browser-mediated login dialog; API keys are
//! written through the same coordinator-owned credential API.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::ProviderEntry;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{SetAgentApiKey, StartAgentLogin};

use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogBody, DialogContent, DialogHeader, DialogTitle,
    FieldError, IconType, TextField,
};
use crate::pump::{Pump, use_store};

use super::agent_login_dialog::AgentLoginDialog;

/// Connects or changes one provider credential.
#[component]
pub fn AgentConnectDialog(
    provider: ProviderEntry,
    #[props(default)] change_key: bool,
    on_close: EventHandler<bool>,
) -> Element {
    let pump = use_store();
    let mut api_key = use_signal(String::new);
    let error = use_signal(String::new);
    let login_id = use_signal(|| None::<String>);

    if login_id().is_some() {
        let restart_pump = pump.clone();
        let restart_provider = provider.id.clone();
        return rsx! {
            // Rendered through an iterator so the key takes effect: a restart
            // then mounts a fresh dialog whose state and poll loop belong to
            // the new login, instead of re-rendering the old one.
            for id in login_id() {
                AgentLoginDialog {
                    key: "{id}",
                    login_id: id.clone(),
                    on_close: move |completed| on_close.call(completed),
                    on_restart: {
                        let restart_pump = restart_pump.clone();
                        let restart_provider = restart_provider.clone();
                        move |()| begin_oauth(restart_pump.clone(), restart_provider.clone(), login_id, error)
                    },
                }
            }
        };
    }

    let oauth_provider_id = provider.id.clone();
    let save_provider_id = provider.id.clone();
    let oauth_pump = pump.clone();
    let save_pump = pump.clone();
    let provider_name = provider.name.clone();
    let is_oauth = provider.supports_oauth && provider.id != "exa" && !change_key;
    let title = if change_key {
        format!("Change {} API key", provider.name)
    } else {
        format!("Connect {}", provider.name)
    };
    rsx! {
        Dialog {
            open: true,
            on_open_change: move |open: bool| {
                if !open {
                    on_close.call(false);
                }
            },
            DialogContent { test_id: "agent-connect-dialog", class: "sm:max-w-md",
                DialogHeader {
                    DialogTitle { "{title}" }
                }
                DialogBody {
                    div { class: "flex flex-col gap-3",
                        p { class: "text-sm text-muted-foreground",
                            if change_key {
                                "Enter a replacement API key for {provider_name}."
                            } else if is_oauth {
                                "Sign in with your {provider_name} subscription, or paste an API key."
                            } else {
                                "Connect {provider_name} with an API key."
                            }
                        }
                        if is_oauth {
                            Button {
                                variant: ButtonVariant::Default,
                                icon: Some(IconType::LogIn),
                                onclick: move |_| begin_oauth(oauth_pump.clone(), oauth_provider_id.clone(), login_id, error),
                                "Sign in with {provider.name}"
                            }
                            p { class: "text-sm text-muted-foreground", "or use an API key" }
                        }
                        TextField {
                            label: "API key",
                            input_type: "password",
                            autocomplete: "new-password",
                            value: api_key(),
                            on_input: move |value| api_key.set(value),
                        }
                        Button {
                            variant: if is_oauth { ButtonVariant::Outline } else { ButtonVariant::Default },
                            disabled: api_key().trim().is_empty(),
                            onclick: move |_| save_key(save_pump.clone(), save_provider_id.clone(), api_key(), error, on_close),
                            "Save API key"
                        }
                        if !error().is_empty() {
                            FieldError { {error()} }
                        }
                        Button {
                            variant: ButtonVariant::Link,
                            onclick: move |_| on_close.call(false),
                            "Cancel"
                        }
                    }
                }
            }
        }
    }
}

fn begin_oauth(
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

fn save_key(
    pump: Pump,
    provider: String,
    api_key: String,
    error: Signal<String>,
    on_close: EventHandler<bool>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        match pump.rpc().call(&SetAgentApiKey { provider, api_key }).await {
            Ok(()) => on_close.call(true),
            Err(failure) => error.set(format!("Could not save API key: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, provider, api_key, error, on_close);
}
