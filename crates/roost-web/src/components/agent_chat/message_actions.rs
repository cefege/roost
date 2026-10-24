//! Retry and rewind controls attached to transcript messages.
//! Rewind removes later transcript items before restoring its draft.
//! The product open flag and per-opening fence own the confirmation lifecycle.
use dioxus::prelude::*;
use libero::components::{Button, Checkbox, Dialog, Flex, Pictogram, Text};
use libero::hooks::{ModalScope, use_modal};
use std::{cell::RefCell, rc::Rc};

use crate::components::overlay_generation::OverlayGeneration;
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use super::request_ids::new_request_id;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{RewindAgentChat, SubmitAgentChat};

#[component]
pub fn UserMessageActions(
    conversation_id: String,
    item_id: String,
    text: String,
    draft: Signal<String>,
    pump: Pump,
) -> Element {
    let mut confirm_open = use_signal(|| false);
    let mut restore_files = use_signal(|| true);
    let mut rewind_busy = use_signal(|| false);
    let mut rewind_error = use_signal(String::new);
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let rewind_generation = Rc::clone(&generation);
    let rewind_pump = pump.clone();
    let rewind_busy_view = rewind_busy;
    let rewind_error_view = rewind_error;
    let mut rewind_restore_view = restore_files;
    let rewind_dialog = use_modal(move |scope: ModalScope<(u64, String, String)>| {
        let Some((_token, _, _)) = scope.try_args() else {
            return rsx! {};
        };
        let action_pump = rewind_pump.clone();
        let action_generation = Rc::clone(&rewind_generation);
        let mut action_busy = rewind_busy_view;
        let mut action_error = rewind_error_view;
        let action_restore = rewind_restore_view;
        let action_draft = draft;
        rsx! {
            Dialog {
                title: "Rewind conversation?",
                close_button: false,
                "aria-describedby": "agent-chat-rewind-description",
                ondismiss: Callback::new(move |_| !*rewind_busy_view.peek()),
                Text {
                    id: "agent-chat-rewind-description",
                    color: "muted",
                    "Messages after this point will be removed from the conversation."
                }
                if !rewind_error_view().is_empty() {
                    Text { color: "error", role: "alert", "{rewind_error_view()}" }
                }
                Checkbox {
                    label: "Restore files changed after this message",
                    checked: rewind_restore_view(),
                    disabled: Some(rewind_busy_view()),
                    onchange: move |value| rewind_restore_view.set(value),
                }
                Flex { direction: "row", gap: "sm", justify: "end",
                    Button {
                        variant: "outlined",
                        disabled: Some(rewind_busy_view()),
                        onclick: move |_| scope.close(),
                        "Cancel"
                    }
                    Button {
                        variant: "outlined",
                        color: "error",
                        loading: Some(rewind_busy_view()),
                        disabled: Some(rewind_busy_view()),
                        onclick: move |_| {
                            if *action_busy.peek() { return; }
                            let Some((token, conversation_id, item_id)) = scope.try_args() else {
                                return;
                            };
                            action_busy.set(true);
                            action_error.set(String::new());
                            rewind_to(
                                action_pump.clone(),
                                conversation_id,
                                item_id,
                                action_restore(),
                                action_draft,
                                scope,
                                Rc::clone(&action_generation),
                                token,
                                action_busy,
                                action_error,
                            );
                        },
                        "Rewind"
                    }
                }
            }
        }
    });
    let retry_pump = pump.clone();
    let retry_id = conversation_id.clone();
    let retry_item = item_id.clone();
    let retry_text = text.clone();
    let open_conversation_id = conversation_id;
    let open_item_id = item_id;
    let open_generation = Rc::clone(&generation);
    let drop_generation = Rc::clone(&generation);
    use_drop(move || drop_generation.borrow_mut().invalidate());
    rsx! {
        Flex { direction: "row", gap: "xs",
            Button {
                variant: "standard",
                icon: rsx! { Pictogram { icon: pictogram_icons_lucide::rotate_ccw::outlined } },
                onclick: move |_| retry_message(retry_pump.clone(), retry_id.clone(), retry_item.clone(), retry_text.clone()),
                "Retry"
            }
            Button {
                variant: "standard",
                icon: rsx! { Pictogram { icon: pictogram_icons_lucide::history::outlined } },
                onclick: move |_| {
                    if *rewind_busy.peek() { return; }
                    rewind_busy.set(false);
                    rewind_error.set(String::new());
                    restore_files.set(true);
                    confirm_open.set(true);
                    let token = open_generation.borrow_mut().begin();
                    let completion_generation = Rc::clone(&open_generation);
                    let mut product_busy = rewind_busy;
                    let mut product_open = confirm_open;
                    rewind_dialog.open_with((token, open_conversation_id.clone(), open_item_id.clone()))
                        .onresult(move |_| {
                            if completion_generation.borrow_mut().finish(token) {
                                product_open.set(false);
                                product_busy.set(false);
                            }
                        });
                },
                "Rewind to here"
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn retry_message(pump: Pump, conversation_id: String, item_id: String, text: String) {
    wasm_bindgen_futures::spawn_local(async move {
        match pump
            .rpc()
            .call(&RewindAgentChat {
                conversation_id: conversation_id.clone(),
                item_id,
                restore_files: false,
                request_id: new_request_id(),
            })
            .await
        {
            Ok(_) => submit_message(pump, conversation_id, text),
            Err(error) => tracing::warn!(target: "agent_chat", %error, "agent retry rewind failed"),
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn retry_message(_: Pump, _: String, _: String, _: String) {}

#[cfg(target_arch = "wasm32")]
fn rewind_to(
    pump: Pump,
    conversation_id: String,
    item_id: String,
    restore_files: bool,
    mut draft: Signal<String>,
    scope: ModalScope<(u64, String, String)>,
    generation: Rc<RefCell<OverlayGeneration>>,
    token: u64,
    mut busy: Signal<bool>,
    mut error_message: Signal<String>,
) {
    dioxus::core::spawn_forever(async move {
        let result = pump
            .rpc()
            .call(&RewindAgentChat {
                conversation_id,
                item_id,
                restore_files,
                request_id: new_request_id(),
            })
            .await;
        if !generation.borrow().is_current(token) || scope.try_args().is_none() {
            return;
        }
        match result {
            Ok(text) => {
                let Ok(mut current_draft) = draft.try_write() else {
                    return;
                };
                *current_draft = text;
                drop(current_draft);
                scope.close();
            }
            Err(failure) => {
                tracing::warn!(target: "agent_chat", error = %failure, "agent rewind failed");
                if let Ok(mut visible_error) = error_message.try_write() {
                    *visible_error = failure.to_string();
                }
                if let Ok(mut pending) = busy.try_write() {
                    *pending = false;
                }
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn rewind_to(
    _: Pump,
    _: String,
    _: String,
    _: bool,
    _: Signal<String>,
    _: ModalScope<(u64, String, String)>,
    _: Rc<RefCell<OverlayGeneration>>,
    _: u64,
    mut busy: Signal<bool>,
    mut error_message: Signal<String>,
) {
    busy.set(false);
    error_message.set("Rewinding conversations requires the browser client.".to_owned());
}

#[cfg(target_arch = "wasm32")]
fn submit_message(pump: Pump, conversation_id: String, text: String) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = pump
            .rpc()
            .call(&SubmitAgentChat {
                conversation_id,
                text,
                request_id: new_request_id(),
                when_busy: String::new(),
                ..Default::default()
            })
            .await
        {
            tracing::warn!(target: "agent_chat", %error, "agent retry submit failed");
        }
    });
}
