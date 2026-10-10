//! The chat composer: one rounded box holding the draft field and a bar with
//! the model pickers and a single round action — Send, or Stop while a run is
//! live and nothing is typed (typing while it runs steers the agent). Enter
//! sends, Shift+Enter breaks the line. The draft belongs to the surface, so a
//! suggestion can fill it. Sends `AgentChatSubmit` and `AgentChatAbort`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AGENT_SLASH_COMMANDS, ConversationSummary, ModelsCatalog, ProviderEntry, parse_slash_command,
};

use super::model_controls::AgentModelControls;
use super::slash_menu::SlashMenu;
use crate::components::md::{ButtonVariant, Chip, IconButton, IconButtonSize};
use crate::components::settings::agent_connect_dialog::AgentConnectDialog;
use crate::pump::Pump;
use crate::router_state::use_navigate;

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
    let navigate = use_navigate();
    let mut selected_command = use_signal(|| 0_usize);
    let mut menu_dismissed = use_signal(|| false);
    let mut connect_provider = use_signal(|| None::<ProviderEntry>);
    let slash_active = {
        let text = draft();
        text.starts_with('/') && !text.chars().any(char::is_whitespace)
    };
    let current_draft = draft();
    let prefix = current_draft.trim_start_matches('/');
    let visible_commands: Vec<_> = AGENT_SLASH_COMMANDS
        .iter()
        .filter(|command| {
            command.name.starts_with(prefix)
                || command
                    .aliases
                    .iter()
                    .any(|alias| alias.starts_with(prefix))
        })
        .collect();
    let send = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        let conversation = conversation.clone();
        let catalog = catalog.clone();
        move |()| {
            let text = draft.peek().trim().to_string();
            if let Some((command, args)) = parse_slash_command(&text)
                && run_local_command(
                    command,
                    args,
                    &LocalCommandScope {
                        conversation: &conversation,
                        catalog: catalog.as_ref(),
                        pump: pump.clone(),
                        navigate,
                        connect_provider,
                    },
                )
            {
                draft.set(String::new());
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
            if slash_active && !menu_dismissed() && !visible_commands.is_empty() {
                SlashMenu {
                    query: draft().clone(),
                    selected: selected_command().min(visible_commands.len().saturating_sub(1)),
                    on_choose: move |command: &'static roost_protocol::wire::agent_chat::SlashCommand| draft.set(format!("/{0} ", command.name)),
                }
            }
            textarea {
                class: "agent-chat__input",
                "data-testid": "agent-chat-input",
                "aria-label": "Message the agent",
                rows: "1",
                placeholder,
                disabled,
                value: "{draft}",
                onmounted: move |event: MountedEvent| on_field_mounted.call(event.data()),
                oninput: move |event: FormEvent| {
                    draft.set(event.value());
                    selected_command.set(0);
                    menu_dismissed.set(false);
                },
                onkeydown: move |event: KeyboardEvent| {
                    let key = event.key().to_string();
                    if slash_active && !menu_dismissed() && !visible_commands.is_empty() {
                        if key == "ArrowDown" || key == "ArrowUp" {
                            event.prevent_default();
                            let count = visible_commands.len();
                            let current = selected_command();
                            selected_command.set(if key == "ArrowDown" {
                                (current + 1) % count
                            } else {
                                current.checked_sub(1).unwrap_or(count - 1)
                            });
                            return;
                        }
                        if key == "Escape" {
                            event.prevent_default();
                            menu_dismissed.set(true);
                            return;
                        }
                        if key == "Tab" || (key == "Enter" && !event.is_composing()) {
                            event.prevent_default();
                            if let Some(command) = visible_commands.get(selected_command().min(visible_commands.len() - 1)) {
                                draft.set(format!("/{0} ", command.name));
                            }
                            return;
                        }
                    }
                    if key == "Enter" && !event.modifiers().contains(Modifiers::SHIFT) && !event.is_composing() {
                        event.prevent_default();
                        send.call(());
                    }
                },
            }
            div { class: "agent-chat__composer-bar",
                if conversation.mode == "plan" {
                    Chip { label: "Plan mode".to_string(), icon: Some("assignment".to_string()), selected: Some(true),
                        onclick: Some(EventHandler::new({
                            let pump = pump.clone();
                            let conversation_id = conversation_id.clone();
                            move |_| submit_direct_command(pump.clone(), conversation_id.clone(), "/plan".to_string())
                        }))
                    }
                }
                Chip { label: "Advisor".to_string(), icon: Some("psychology".to_string()), selected: Some(conversation.advisor),
                    onclick: Some(EventHandler::new({
                        let pump = pump.clone();
                        let conversation_id = conversation_id.clone();
                        move |_| submit_direct_command(pump.clone(), conversation_id.clone(), "/advisor".to_string())
                    }))
                }
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
            if let Some(provider) = connect_provider() {
                AgentConnectDialog {
                    provider,
                    on_close: move |_| connect_provider.set(None),
                }
            }
        }
    }
}
/// What a browser-run slash command may act on.
struct LocalCommandScope<'a> {
    conversation: &'a ConversationSummary,
    catalog: Option<&'a ModelsCatalog>,
    pump: Pump,
    navigate: EventHandler<String>,
    connect_provider: Signal<Option<ProviderEntry>>,
}

/// Runs a client-side command; `false` means the text goes to the harness.
fn run_local_command(
    command: &roost_protocol::wire::agent_chat::SlashCommand,
    args: &str,
    scope: &LocalCommandScope<'_>,
) -> bool {
    let (name, client_bare) = (command.name, command.client_bare);
    let conversation = scope.conversation;
    let catalog = scope.catalog;
    let pump = scope.pump.clone();
    let navigate = scope.navigate;
    let mut connect_provider = scope.connect_provider;
    match name {
        "model" if client_bare && args.is_empty() => {
            #[cfg(target_arch = "wasm32")]
            open_model_picker();
            true
        }
        "login" => {
            let provider = catalog.and_then(|catalog| {
                if args.is_empty() {
                    catalog.providers.first()
                } else {
                    catalog
                        .providers
                        .iter()
                        .find(|provider| provider.id == args)
                }
            });
            if let Some(provider) = provider {
                connect_provider.set(Some(provider.clone()));
            }
            true
        }
        "logout" => {
            navigate.call(crate::routes::settings_pane_href("models"));
            true
        }
        "new" => {
            crate::components::agent_chat::launch_agent(
                pump,
                conversation.worker_fp.clone(),
                conversation.cwd.clone(),
                navigate,
            );
            true
        }
        _ => false,
    }
}

#[cfg(target_arch = "wasm32")]
fn open_model_picker() {
    use wasm_bindgen::JsCast;
    if let Some(button) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("agent-chat-model-trigger"))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        button.click();
    }
}

#[cfg(target_arch = "wasm32")]
fn submit_direct_command(pump: Pump, conversation_id: String, command: String) {
    submit_message(pump, conversation_id, command);
}

#[cfg(not(target_arch = "wasm32"))]
fn submit_direct_command(_: Pump, _: String, _: String) {}

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
