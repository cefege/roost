//! A focused credential flow for one agent model provider.
//! OAuth stays in the existing browser-mediated login dialog; API keys are
//! written through the same coordinator-owned credential API.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::ProviderEntry;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{SetAgentApiKey, StartAgentLogin};

use crate::pump::{Pump, use_store};
use libero::components::{Button, Dialog, Flex, Pictogram, Text, TextField};
use libero::hooks::{ModalScope, use_modal};

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
    let busy = use_signal(|| false);
    let login_id = use_signal(|| None::<String>);
    let provider_name = provider.name.clone();
    let title = if change_key {
        format!("Change {} API key", provider.name)
    } else {
        format!("Connect {}", provider.name)
    };
    let is_oauth = provider.supports_oauth && provider.id != "exa" && !change_key;
    let oauth_provider_id = provider.id.clone();
    let save_provider_id = provider.id.clone();
    let oauth_pump = pump.clone();
    let save_pump = pump.clone();
    let dialog = use_modal(move |scope: ModalScope<(), bool>| {
        let title = title.clone();
        let provider_name = provider_name.clone();
        let oauth_pump = oauth_pump.clone();
        let save_pump = save_pump.clone();
        let oauth_provider_id = oauth_provider_id.clone();
        let save_provider_id = save_provider_id.clone();
        rsx! {
            Dialog { title, close_button: false, ondismiss: move |_| !busy(),
                Flex { direction: "column", gap: "md",
                    Text { size: "sm", color: "muted",
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
                            variant: "filled",
                            icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::log_in::outlined } }),
                            disabled: busy(),
                            loading: busy(),
                            onclick: move |_| begin_oauth(oauth_pump.clone(), oauth_provider_id.clone(), login_id, error, busy),
                            "Sign in with {provider_name}"
                        }
                        Text { size: "sm", color: "muted", "or use an API key" }
                    }
                    TextField {
                        label: "API key",
                        r#type: "password",
                        autocomplete: "new-password",
                        value: Some(api_key()),
                        oninput: move |value| api_key.set(value),
                    }
                    Button {
                        variant: if is_oauth { "outlined" } else { "filled" },
                        disabled: api_key().trim().is_empty() || busy(),
                        loading: busy(),
                        onclick: move |_| save_key(save_pump.clone(), save_provider_id.clone(), api_key(), error, busy, scope),
                        if busy() { "Saving…" } else { "Save API key" }
                    }
                    if !error().is_empty() {
                        Text { size: "sm", color: "error", role: "alert", "{error()}" }
                    }
                    Button { variant: "standard", color: "muted", disabled: busy(), onclick: move |_| scope.resolve(false), "Cancel" }
                }
            }
        }
    });
    let open_dialog = dialog;
    use_effect(move || {
        open_dialog.open().onresult(move |result| {
            let Ok(handing_off) = login_id.try_peek().map(|login| login.is_some()) else {
                return;
            };
            match result {
                Some(completed) => on_close.call(completed),
                None if !handing_off => on_close.call(false),
                None => {}
            }
        });
    });
    let close_oauth_dialog = dialog;
    use_effect(move || {
        if login_id().is_some() {
            close_oauth_dialog.close();
        }
    });
    if login_id().is_some() {
        let restart_pump = pump.clone();
        let restart_provider = provider.id.clone();
        return rsx! {
            for id in login_id() {
                AgentLoginDialog {
                    key: "{id}",
                    login_id: id.clone(),
                    on_close: move |completed| on_close.call(completed),
                    on_restart: {
                        let restart_pump = restart_pump.clone();
                        let restart_provider = restart_provider.clone();
                        move |()| begin_oauth(restart_pump.clone(), restart_provider.clone(), login_id, error, busy)
                    },
                }
            }
        };
    }
    rsx! {}
}

fn begin_oauth(
    pump: Pump,
    provider: String,
    login_id: Signal<Option<String>>,
    error: Signal<String>,
    busy: Signal<bool>,
) {
    let mut busy = busy;
    busy.set(true);
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut login_id = login_id;
        let mut error = error;
        let mut busy = busy;
        let result = pump.rpc().call(&StartAgentLogin { provider }).await;
        if login_id.try_peek().is_err() || error.try_peek().is_err() || busy.try_peek().is_err() {
            return;
        }
        match result {
            Ok(id) => {
                login_id.set(Some(id));
                error.set(String::new());
            }
            Err(failure) => {
                error.set(format!("Could not start sign-in: {failure}"));
            }
        }
        busy.set(false);
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        busy.set(false);
        let _ = (pump, provider, login_id, error, busy);
    }
}

fn save_key(
    pump: Pump,
    provider: String,
    api_key: String,
    error: Signal<String>,
    busy: Signal<bool>,
    scope: ModalScope<(), bool>,
) {
    let mut busy = busy;
    busy.set(true);
    #[cfg(target_arch = "wasm32")]
    // The credential write outlives the opening; Libero's resolution still needs a Dioxus runtime.
    dioxus::core::spawn_forever(async move {
        let mut error = error;
        let mut busy = busy;
        let result = pump.rpc().call(&SetAgentApiKey { provider, api_key }).await;
        if scope.try_args().is_none() {
            return;
        }
        match result {
            Ok(()) => scope.resolve(true),
            Err(failure) => {
                busy.set(false);
                error.set(format!("Could not save API key: {failure}"));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        busy.set(false);
        let _ = (pump, provider, api_key, error, busy, scope);
    }
}
