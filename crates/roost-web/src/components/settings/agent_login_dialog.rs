//! Browser-mediated provider login for the built-in agent.
//!
//! Polling belongs to the mounted dialog: closing it stops future requests, and
//! a completed login asks the parent to refresh its provider catalog.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{
    CancelAgentLogin, PollAgentLogin, RespondAgentLogin,
};

use crate::components::md::{Button, ButtonVariant, Dialog, TextField};
use crate::pump::{Pump, use_store};
use roost_protocol::wire::agent_chat::{LoginPromptType, LoginState, LoginStatus};

/// Login ceremony identified by the host's opaque id.
#[component]
pub fn AgentLoginDialog(login_id: String, on_close: EventHandler<bool>) -> Element {
    let pump = use_store();
    let state = use_signal(|| None::<LoginState>);
    let mut answer = use_signal(String::new);
    let error = use_signal(String::new);
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
    let prompt = current.as_ref().and_then(|value| value.prompt.clone());
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
    let error_message = if !error().is_empty() {
        error()
    } else {
        current
            .as_ref()
            .and_then(|value| value.error.clone())
            .unwrap_or_default()
    };

    rsx! {
        Dialog {
            open: true,
            headline: "Sign in to provider".to_owned(),
            test_id: "agent-login-dialog",
            on_close: move |_| cancel_login(cancel_pump.clone(), close_id.clone(), on_close, error),
            div { class: "agent-settings__dialog-body",
                if let Some(value) = current {
                    for notice in value.notices {
                        div {
                            key: "{notice.notice_type:?}-{notice.message}",
                            class: "agent-settings__notice-card",
                            p { class: "md-body-m", "{notice.message}" }
                            if notice.notice_type == roost_protocol::wire::agent_chat::LoginNoticeType::AuthUrl {
                                if let Some(url) = notice.url {
                                    a {
                                        class: "roost-button roost-button--link",
                                        href: "{url}",
                                        target: "_blank",
                                        rel: "noopener noreferrer",
                                        "Open sign-in page"
                                    }
                                }
                            }
                            if notice.notice_type == roost_protocol::wire::agent_chat::LoginNoticeType::DeviceCode {
                                if let Some(code) = notice.code {
                                    p { class: "agent-settings__device-code", "{code}" }
                                }
                            }
                        }
                    }
                    if value.state == LoginStatus::Waiting { p { class: "md-body-s", "Waiting for the provider…" } }
                    if value.state == LoginStatus::Done { p { class: "md-body-s", "Sign-in complete." } }
                    if value.state == LoginStatus::Failed {
                        p { class: "agent-settings__notice md-body-s", role: "alert", {value.error.unwrap_or_else(|| "Sign-in failed.".to_owned())} }
                    }
                } else {
                    p { class: "md-body-s", "Connecting to provider…" }
                }
                if !error_message.is_empty() {
                    p { class: "agent-settings__notice md-body-s", role: "alert", {error_message} }
                }
                if let Some(prompt) = prompt {
                    if prompt.prompt_type == LoginPromptType::ManualCode {
                        p { class: "agent-settings__manual-instruction md-body-m",
                            "Enter the code shown by the provider to finish signing in."
                        }
                    }
                    p { class: "md-body-s", {prompt.message} }
                    if prompt.prompt_type == LoginPromptType::Select {
                        div { class: "agent-settings__select-options", role: "group", "aria-label": "Choose an option",
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
                        disabled: answer().is_empty(),
                        onclick: move |_| respond_login(submit_pump.clone(), submit_id.clone(), prompt_id.clone(), answer(), error),
                        "Submit"
                    }
                }
                Button {
                    variant: ButtonVariant::Outline,
                    onclick: move |_| cancel_login(cancel_button_pump.clone(), cancel_id.clone(), on_close, error),
                    "Cancel"
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
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        if let Err(failure) = pump
            .rpc()
            .call(&RespondAgentLogin {
                login_id,
                prompt_id,
                value,
            })
            .await
        {
            error.set(format!("Could not submit sign-in response: {failure}"));
        } else {
            error.set(String::new());
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, login_id, prompt_id, value, error);
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
