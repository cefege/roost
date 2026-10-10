//! The chat composer: one rounded box holding the draft field and a bar with
//! the model pickers and a single round action — Send, or Stop while a run is
//! live and nothing is typed (typing while it runs steers the agent). Enter
//! sends, Shift+Enter breaks the line. The draft belongs to the surface, so a
//! suggestion can fill it. Sends `AgentChatSubmit` and `AgentChatAbort`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::model_controls::AgentModelControls;
use crate::components::md::{ButtonVariant, IconButton, IconButtonSize};
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{AbortAgentChat, SubmitAgentChat};

#[component]
pub fn AgentComposer(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    draft: Signal<String>,
    running: bool,
    host_connected: bool,
    pump: Pump,
    on_field_mounted: EventHandler<std::rc::Rc<MountedData>>,
) -> Element {
    let mut draft = draft;
    let disabled = !host_connected;
    let conversation_id = conversation.id.clone();
    let send = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        move |()| {
            let text = draft.peek().trim().to_string();
            if text.is_empty() || disabled {
                return;
            }
            #[cfg(target_arch = "wasm32")]
            submit_message(pump.clone(), conversation_id.clone(), text);
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (&pump, &conversation_id, text);
            draft.set(String::new());
        }
    });
    let stop = {
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        move |_| {
            #[cfg(target_arch = "wasm32")]
            abort_run(pump.clone(), conversation_id.clone());
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (&pump, &conversation_id);
        }
    };
    let empty = draft().trim().is_empty();
    let placeholder = if disabled {
        "The agent host is offline"
    } else if running {
        "Steer the agent…"
    } else {
        "Ask the agent to do something…"
    };
    rsx! {
        div {
            class: "agent-chat__composer",
            "data-disabled": disabled.then_some("true"),
            "data-testid": "agent-chat-composer",
            textarea {
                class: "agent-chat__input",
                "data-testid": "agent-chat-input",
                "aria-label": "Message the agent",
                rows: "1",
                placeholder,
                disabled,
                value: "{draft}",
                onmounted: move |event: MountedEvent| on_field_mounted.call(event.data()),
                oninput: move |event: FormEvent| draft.set(event.value()),
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Enter
                        && !event.modifiers().contains(Modifiers::SHIFT)
                        && !event.is_composing()
                    {
                        event.prevent_default();
                        send.call(());
                    }
                },
            }
            div { class: "agent-chat__composer-bar",
                AgentModelControls { conversation: conversation.clone(), catalog, pump: pump.clone() }
                span { class: "agent-chat__composer-spacer" }
                if running && empty {
                    IconButton {
                        icon: "stop",
                        label: "Stop the agent",
                        title: "Stop",
                        variant: ButtonVariant::Secondary,
                        size: IconButtonSize::Icon,
                        class: "agent-chat__action",
                        "data-testid": "agent-chat-stop",
                        onclick: stop,
                    }
                } else {
                    IconButton {
                        icon: "arrow_upward",
                        label: if running { "Steer the agent" } else { "Send message" },
                        title: if running { "Steer" } else { "Send" },
                        variant: ButtonVariant::Default,
                        size: IconButtonSize::Icon,
                        class: "agent-chat__action",
                        "data-testid": "agent-chat-send",
                        "aria-disabled": (disabled || empty).then_some("true"),
                        onclick: move |_| send.call(()),
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn submit_message(pump: Pump, conversation_id: String, text: String) {
    let mut bytes = [0_u8; 16];
    if let Some(crypto) = web_sys::window().and_then(|window| window.crypto().ok()) {
        let _ = crypto.get_random_values_with_u8_array(&mut bytes);
    }
    let request_id = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
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

#[cfg(target_arch = "wasm32")]
fn abort_run(pump: Pump, conversation_id: String) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = pump.rpc().call(&AbortAgentChat { conversation_id }).await {
            tracing::warn!(target: "agent_chat", %error, "agent abort failed");
        }
    });
}
