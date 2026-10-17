//! Browser-mediated provider login for the built-in agent.
//!
//! Polling belongs to the mounted dialog: closing it stops future requests, and
//! a completed login asks the parent to refresh its provider catalog.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{
    CancelAgentLogin, PollAgentLogin, RespondAgentLogin,
};

use crate::components::agent_chat::agent_alert::ERROR_TEXT_CLASS;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Dialog, DialogBody, DialogContent, DialogHeader,
    DialogTitle, IconButton, IconType, Surface, SurfaceRadius, TextField,
};
use crate::pump::{Pump, use_store};
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
    let cancel_pump = pump.clone();
    let submit_pump = pump.clone();
    let cancel_button_pump = pump.clone();
    let close_id = login_id.clone();
    let submit_id = login_id.clone();
    let cancel_id = login_id.clone();
    // The login's own failure is drawn once, in its Failed block; this line is
    // only for the calls this dialog makes.
    let error_message = if ended { String::new() } else { error() };

    rsx! {
        Dialog {
            open: true,
            on_open_change: move |open: bool| {
                if !open {
                    cancel_login(cancel_pump.clone(), close_id.clone(), on_close, error);
                }
            },
            DialogContent { test_id: "agent-login-dialog",
                DialogHeader {
                    DialogTitle { "Sign in to provider" }
                }
                DialogBody {
                    div { class: "flex flex-col gap-3",
                        if let Some(value) = current {
                            for notice in value.notices {
                                Surface {
                                    key: "{notice.notice_type:?}-{notice.message}",
                                    border: true,
                                    pad: Some(3),
                                    radius: SurfaceRadius::Md,
                                    p { class: "text-base", "{notice.message}" }
                                    if notice.notice_type == roost_protocol::wire::agent_chat::LoginNoticeType::AuthUrl {
                                        if let Some(url) = notice.url {
                                            SignInUrl { url }
                                        }
                                    }
                                    if notice.notice_type == roost_protocol::wire::agent_chat::LoginNoticeType::DeviceCode {
                                        if let Some(code) = notice.code {
                                            p { class: "font-mono text-xl tracking-widest", "{code}" }
                                        }
                                    }
                                }
                            }
                            if value.state == LoginStatus::Waiting { p { class: "text-sm text-muted-foreground", "Waiting for the provider…" } }
                            if value.state == LoginStatus::Done { p { class: "text-sm text-muted-foreground", "Sign-in complete." } }
                            if value.state == LoginStatus::Failed {
                                div { class: "flex flex-col gap-2 rounded-md border border-destructive/40 p-3", role: "alert",
                                    p { class: ERROR_TEXT_CLASS, {value.error.unwrap_or_else(|| "Sign-in failed.".to_owned())} }
                                    p { class: "text-sm text-muted-foreground",
                                        "A code works once, and only for the most recent sign-in link. Start over for a fresh link."
                                    }
                                    Button {
                                        icon: Some(IconType::RefreshCw),
                                        "data-testid": "agent-login-restart",
                                        onclick: move |_| on_restart.call(()),
                                        "Start over"
                                    }
                                }
                            }
                        } else {
                            p { class: "text-sm text-muted-foreground", "Connecting to provider…" }
                        }
                        if !error_message.is_empty() {
                            p { class: ERROR_TEXT_CLASS, role: "alert", {error_message} }
                        }
                        if let Some(prompt) = prompt {
                            if prompt.prompt_type == LoginPromptType::ManualCode {
                                p { class: "text-base font-medium",
                                    "Enter the code shown by the provider to finish signing in."
                                }
                            }
                            p { class: "text-sm text-muted-foreground", {prompt.message} }
                            if prompt.prompt_type == LoginPromptType::Select {
                                div { class: "flex flex-col gap-2", role: "group", "aria-label": "Choose an option",
                                    for option in prompt.options {
                                        {
                                            let selected = answer() == option;
                                            let option_answer = option.clone();
                                            rsx! {
                                                Button {
                                                    key: "{option}",
                                                    variant: if selected { ButtonVariant::Secondary } else { ButtonVariant::Outline },
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
                                    input_type: if prompt_type == Some(LoginPromptType::Secret) { "password" } else { "text" },
                                    value: answer(),
                                    on_input: move |value| answer.set(value),
                                }
                            }
                            Button {
                                disabled: answer().trim().is_empty() || submitting(),
                                onclick: move |_| respond_login(submit_pump.clone(), submit_id.clone(), prompt_id.clone(), answer(), error, submitting),
                                if submitting() { "Submitting…" } else { "Submit" }
                            }
                        }
                        Button {
                            variant: ButtonVariant::Outline,
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
        }
    }
}

/// The provider's sign-in address in full, as a link, with a copy button for
/// opening it in another browser or on another device.
#[component]
fn SignInUrl(url: String) -> Element {
    let mut copied = use_signal(|| false);
    rsx! {
        Surface {
            class: "flex items-center gap-2 break-all",
            pad: Some(2),
            radius: SurfaceRadius::Sm,
            a {
                class: "min-w-0 flex-1 text-primary underline",
                href: "{url}",
                target: "_blank",
                rel: "noopener noreferrer",
                "data-testid": "agent-login-url",
                "{url}"
            }
            IconButton {
                icon: if copied() { IconType::Check } else { IconType::Copy },
                label: "Copy sign-in link",
                title: if copied() { "Copied" } else { "Copy link" },
                size: ButtonSize::IconSm,
                "data-testid": "agent-login-url-copy",
                onclick: {
                    let url = url.clone();
                    move |_| {
                        crate::components::notifications::clipboard::copy_text_then(&url, move |written| copied.set(written));
                    }
                },
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
