//! Retry and rewind controls attached to transcript messages.
//! Rewind changes the transcript before any draft is restored to the composer.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, Checkbox, Dialog};
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
    let mut confirm = use_signal(|| false);
    let mut restore_files = use_signal(|| true);
    let retry_pump = pump.clone();
    let retry_id = conversation_id.clone();
    let retry_item = item_id.clone();
    let retry_text = text.clone();
    let rewind_pump = pump.clone();
    let rewind_id = conversation_id.clone();
    let rewind_item = item_id.clone();
    rsx! {
        div { class: "agent-chat__message-actions",
            Button { variant: ButtonVariant::Ghost, icon: "replay", onclick: move |_| retry_message(retry_pump.clone(), retry_id.clone(), retry_item.clone(), retry_text.clone()), "Retry" }
            Button { variant: ButtonVariant::Ghost, icon: "history", onclick: move |_| confirm.set(true), "Rewind to here" }
        }
        if confirm() {
            Dialog {
                open: true,
                headline: "Rewind conversation?".to_string(),
                on_close: move |_| confirm.set(false),
                actions: rsx! {
                    Button { variant: ButtonVariant::Outline, onclick: move |_| confirm.set(false), "Cancel" }
                    Button { variant: ButtonVariant::Destructive, onclick: move |_| {
                        confirm.set(false);
                        rewind_to(rewind_pump.clone(), rewind_id.clone(), rewind_item.clone(), restore_files(), draft);
                    }, "Rewind" }
                },
                div { class: "agent-chat__rewind-confirm",
                    p { class: "md-body-m", "Messages after this point will be removed from the conversation." }
                    Checkbox { checked: restore_files(), label: "Restore files changed after this message".to_string(), on_change: move |value| restore_files.set(value) }
                }
            }
        }
    }
}

fn retry_message(pump: Pump, conversation_id: String, item_id: String, text: String) {
    #[cfg(target_arch = "wasm32")]
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
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, conversation_id, item_id, text);
}

#[allow(unused_mut)]
fn rewind_to(
    pump: Pump,
    conversation_id: String,
    item_id: String,
    restore_files: bool,
    mut draft: Signal<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump
            .rpc()
            .call(&RewindAgentChat {
                conversation_id,
                item_id,
                restore_files,
                request_id: new_request_id(),
            })
            .await
        {
            Ok(text) => draft.set(text),
            Err(error) => tracing::warn!(target: "agent_chat", %error, "agent rewind failed"),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, conversation_id, item_id, restore_files, draft);
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
                ..Default::default()
            })
            .await
        {
            tracing::warn!(target: "agent_chat", %error, "agent retry submit failed");
        }
    });
}
