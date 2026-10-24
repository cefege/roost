//! Browser-mediated provider login for the built-in agent.
//!
//! Polling belongs to the mounted dialog: closing it stops future requests, and
//! a completed login asks the parent to refresh its provider catalog.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{
    CancelAgentLogin, PollAgentLogin, RespondAgentLogin,
};

use crate::pump::{Pump, use_store};
use libero::components::{Alert, Button, Dialog, Flex, Paper, Pictogram, Text, TextField};
use libero::hooks::{ModalScope, use_modal};
use libero::sx::sx;
use roost_protocol::wire::agent_chat::{LoginPromptType, LoginState, LoginStatus};

/// Login ceremony identified by the host's opaque id.
#[component]
pub fn AgentLoginDialog(
    login_id: String,
    on_close: EventHandler<bool>,
    /// Begin a fresh login, replacing this one; a failed or expired code is
    /// never retried against the same login.
    on_restart: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let state = use_signal(|| None::<LoginState>);
    let mut answer = use_signal(String::new);
    let error = use_signal(String::new);
    let submitting = use_signal(|| false);
    let mut alive = use_signal(|| true);
    use_drop(move || alive.set(false));

    #[cfg(target_arch = "wasm32")]
    let poll_pump = pump.clone();
    #[cfg(target_arch = "wasm32")]
    let poll_id = login_id.clone();
    #[cfg(target_arch = "wasm32")]
    let poll_alive = alive;
    #[cfg(target_arch = "wasm32")]
    let poll_state = state;
    #[cfg(target_arch = "wasm32")]
    let poll_error = error;
    #[cfg(target_arch = "wasm32")]
    let complete = on_close;
    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            let poll_pump = poll_pump.clone();
            let poll_id = poll_id.clone();
            let poll_alive = poll_alive;
            let mut poll_state = poll_state;
            let mut poll_error = poll_error;
            let complete = complete;
            wasm_bindgen_futures::spawn_local(async move {
                while poll_alive() {
                    match poll_pump
                        .rpc()
                        .call(&PollAgentLogin {
                            login_id: poll_id.clone(),
                        })
                        .await
                    {
                        Ok(value) => {
                            let finished = value.state == LoginStatus::Done;
                            let failed = value.state == LoginStatus::Failed;
                            poll_state.set(Some(value));
                            if finished {
                                complete.call(true);
                                break;
                            }
                            if failed {
                                break;
                            }
                            wait_one_second().await;
                        }
                        Err(failure) => {
                            poll_error.set(format!("Could not check sign-in: {failure}"));
                            wait_one_second().await;
                        }
                    }
                }
            });
        }
    });

    let current = state();
    let status = current.as_ref().map(|value| value.state);
    let ended = matches!(status, Some(LoginStatus::Failed | LoginStatus::Done));
    // A finished login takes no more answers: its prompt would only collect a
    // code the coordinator then refuses.
    let prompt = current
        .as_ref()
        .and_then(|value| value.prompt.clone())
        .filter(|_| !ended);
    let prompt_id = prompt
        .as_ref()
        .map(|value| value.id.clone())
        .unwrap_or_default();
    let prompt_type = prompt.as_ref().map(|value| value.prompt_type);
    let submit_pump = pump.clone();
    let cancel_button_pump = pump.clone();
    let submit_id = login_id.clone();
    let cancel_id = login_id.clone();
    // The login's own failure is drawn once, in its Failed block; this line is
    // only for the calls this dialog makes.
    let error_message = if ended { String::new() } else { error() };
    let code_sx = use_hook(|| {
        sx().font_family("monospace")
            .with("letter-spacing", "0.12em")
    });
    let notice_sx = use_hook(|| sx().padding("md"));

    let dialog = use_modal(move |_: ModalScope<(), bool>| {
        let current = current.clone();
        let prompt = prompt.clone();
        let submit_pump = submit_pump.clone();
        let cancel_button_pump = cancel_button_pump.clone();
        let submit_id = submit_id.clone();
        let cancel_id = cancel_id.clone();
        let prompt_id = prompt_id.clone();
        let error_message = error_message.clone();
        let code_sx = code_sx.clone();
        let notice_sx = notice_sx.clone();
        rsx! {
        Dialog { title: "Sign in to provider", close_button: false, ondismiss: move |_| false,
            Flex { direction: "column", gap: "md", "data-testid": "agent-login-dialog",
                if let Some(value) = current.clone() {
                    for notice in value.notices {
                        Paper { key: "{notice.notice_type:?}-{notice.message}", sx: notice_sx.clone(),
                            Text { size: "md", "{notice.message}" }
                            if notice.notice_type == roost_protocol::wire::agent_chat::LoginNoticeType::AuthUrl {
                                if let Some(url) = notice.url { SignInUrl { url } }
                            }
                            if notice.notice_type == roost_protocol::wire::agent_chat::LoginNoticeType::DeviceCode {
                                if let Some(code) = notice.code {
                                    Text { size: "lg", sx: code_sx.clone(), "{code}" }
                                }
                            }
                        }
                    }
                    if value.state == LoginStatus::Waiting { Text { size: "sm", color: "muted", "Waiting for the provider…" } }
                    if value.state == LoginStatus::Done { Text { size: "sm", color: "muted", "Sign-in complete." } }
                    if value.state == LoginStatus::Failed {
                        Alert { color: "error", title: value.error.unwrap_or_else(|| "Sign-in failed.".to_owned()),
                            Text { size: "sm", color: "muted",
                                "A code works once, and only for the most recent sign-in link. Start over for a fresh link."
                            }
                        }
                        Button {
                            icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::refresh_cw::outlined } }),
                            "data-testid": "agent-login-restart",
                            onclick: move |_| on_restart.call(()),
                            "Start over"
                        }
                    }
                } else {
                    Text { size: "sm", color: "muted", "Connecting to provider…" }
                }
                if !error_message.is_empty() {
                    Text { size: "sm", color: "error", role: "alert", "{error_message}" }
                }
                if let Some(prompt) = prompt.clone() {
                    if prompt.prompt_type == LoginPromptType::ManualCode {
                        Text { size: "md", "Enter the code shown by the provider to finish signing in." }
                    }
                    Text { size: "sm", color: "muted", "{prompt.message}" }
                    if prompt.prompt_type == LoginPromptType::Select {
                        Flex { direction: "column", gap: "sm", role: "group", "aria-label": "Choose an option",
                            for option in prompt.options {
                                {
                                    let selected = answer() == option;
                                    let option_answer = option.clone();
                                    rsx! {
                                        Button {
                                            key: "{option}",
                                            selected: Some(selected),
                                            variant: if selected { "filled" } else { "outlined" },
                                            onclick: move |_| answer.set(option_answer.clone()),
                                            "{option}"
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        TextField {
                            label: "Response",
                            r#type: if prompt_type == Some(LoginPromptType::Secret) { "password" } else { "text" },
                            value: Some(answer()),
                            oninput: move |value| answer.set(value),
                        }
                    }
                    Button {
                        disabled: answer().trim().is_empty() || submitting(),
                        loading: submitting(),
                        onclick: move |_| respond_login(submit_pump.clone(), submit_id.clone(), prompt_id.clone(), answer(), error, submitting),
                        if submitting() { "Submitting…" } else { "Submit" }
                    }
                }
                Button {
                    variant: "outlined",
                    disabled: submitting(),
                    onclick: move |_| {
                        if ended {
                            on_close.call(status == Some(LoginStatus::Done));
                        } else {
                            cancel_login(cancel_button_pump.clone(), cancel_id.clone(), on_close, error);
                        }
                    },
                    if ended { "Close" } else { "Cancel" }
                }
            }
        }
        }
    });
    use_effect(move || {
        dialog.open();
    });
    rsx! {}
}

/// The provider's sign-in address in full, as a link, with a copy button for
/// opening it in another browser or on another device.
#[component]
fn SignInUrl(url: String) -> Element {
    let (copy_url, copied) = crate::components::hooks::use_copy_clipboard::use_copy_clipboard(None);
    let link_card_sx = use_hook(|| sx().padding("sm"));
    let link_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .with("overflow-wrap", "anywhere")
            .color("primary")
            .with("text-decoration", "underline")
    });
    rsx! {
        Paper { sx: link_card_sx,
            Flex { direction: "row", align: "center", gap: "sm",
                Text {
                    component: "a",
                    sx: link_sx,
                    "href": "{url}",
                    "target": "_blank",
                    "rel": "noopener noreferrer",
                    "data-testid": "agent-login-url",
                    "{url}"
                }
                Button {
                    variant: "standard",
                    icon: Some(rsx! { Pictogram { icon: if copied() { pictogram_icons_lucide::check::outlined } else { pictogram_icons_lucide::copy::outlined } } }),
                    title: if copied() { "Copied" } else { "Copy link" },
                    "aria-label": "Copy sign-in link",
                    "data-testid": "agent-login-url-copy",
                    onclick: {
                        let url = url.clone();
                        move |_| copy_url(&url)
                    },
                    if copied() { "Copied" } else { "Copy" }
                }
            }
        }
    }
}

fn respond_login(
    pump: Pump,
    login_id: String,
    prompt_id: String,
    value: String,
    error: Signal<String>,
    submitting: Signal<bool>,
) {
    let mut submitting = submitting;
    submitting.set(true);
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        let result = pump
            .rpc()
            .call(&RespondAgentLogin {
                login_id,
                prompt_id,
                value: value.trim().to_owned(),
            })
            .await;
        match result {
            Ok(()) => error.set(String::new()),
            Err(failure) => error.set(format!("Could not submit the code: {failure}")),
        }
        if let Ok(mut busy) = submitting.try_write() {
            *busy = false;
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, login_id, prompt_id, value, error, submitting);
}

fn cancel_login(pump: Pump, login_id: String, on_close: EventHandler<bool>, error: Signal<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        match pump.rpc().call(&CancelAgentLogin { login_id }).await {
            Ok(()) => on_close.call(false),
            Err(failure) => error.set(format!("Could not cancel sign-in: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, login_id, on_close, error);
}

#[cfg(target_arch = "wasm32")]
async fn wait_one_second() {
    use wasm_bindgen::{JsCast, JsValue, closure::Closure};
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let callback = Closure::once_into_js(move || {
                let _ = resolve.call0(&JsValue::NULL);
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                1_000,
            );
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
