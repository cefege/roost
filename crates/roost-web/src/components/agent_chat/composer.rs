//! The single chat input and submit action.
//! Busy runs accept steering input; offline hosts disable the whole composer.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, TextField};
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::SubmitAgentChat;

#[component]
pub fn AgentComposer(
    conversation_id: String,
    running: bool,
    host_connected: bool,
    pump: Pump,
) -> Element {
    let mut draft = use_signal(String::new);
    let disabled = !host_connected;
    let send = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        move |()| {
            let text = draft().trim().to_string();
            if text.is_empty() || disabled {
                return;
            }
            #[cfg(target_arch = "wasm32")]
            {
                let pump = pump.clone();
                let conversation_id = conversation_id.clone();
                let request_id = format!(
                    "{:016x}",
                    (js_sys::Date::now() * js_sys::Math::random()) as u64
                );
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(error) = pump
                        .rpc()
                        .call(&SubmitAgentChat {
                            conversation_id,
                            text,
                            request_id,
                        })
                        .await
                    {
                        tracing::warn!(target: "agent_chat", %error, "agent message submit failed");
                    }
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (&pump, &conversation_id, text);
            draft.set(String::new());
        }
    });
    let send_from_key = send;
    let send_from_button = send;
    let on_keydown = EventHandler::new(move |event: KeyboardEvent| {
        if event.key() == Key::Enter && !event.modifiers().contains(Modifiers::SHIFT) {
            event.prevent_default();
            send_from_key.call(());
        }
    });
    rsx! {
        div { class: "agent-chat__composer",
            TextField {
                value: draft(), on_input: move |value| draft.set(value),
                input_type: Some("textarea".to_string()), rows: Some(3),
                placeholder: Some(if disabled { "Agent host offline".to_string() } else if running { "Steer the agent…".to_string() } else { "Message the agent…".to_string() }),
                aria_label: Some("Message the agent".to_string()), disabled,
                onkeydown: Some(on_keydown), class: Some("agent-chat__composer-field".to_string()),
            }
            Button { variant: ButtonVariant::Default, icon: Some("send".to_string()), disabled: disabled,
                onclick: move |_| send_from_button.call(()), "Send"
            }
        }
    }
}
