//! The chat composer: one rounded box holding the draft field and a bar with
//! the model pickers and a single round action — Send, or Stop while a run is
//! live and nothing is typed (typing while it runs steers the agent). Enter
//! sends, Shift+Enter breaks the line. While the draft names a command, Enter
//! and Tab complete the highlighted option from the command menu. The draft
//! belongs to the surface, so a suggestion can fill it. Sends
//! `AgentChatSubmit` and `AgentChatAbort`.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, ProviderEntry, Transcript, parse_slash_command,
};

use super::model_controls::AgentModelControls;
use super::slash_menu::{
    SlashMenu, effort_levels, initial_selection, listbox_id, option_id, scroll_option_into_view,
    slash_menu_state,
};
use crate::components::md::{ButtonVariant, Chip, Icon, IconButton, IconButtonSize, IconSize};
use crate::components::settings::agent_connect_dialog::AgentConnectDialog;
use crate::pump::Pump;
use crate::router_state::use_navigate;

use super::composer_commands::{
    LocalCommandScope, advisor_title, report_command, run_local_command, submit_direct_command,
    submit_message,
};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::AbortAgentChat;

#[component]
pub fn AgentComposer(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    catalog_failed: bool,
    transcript: Transcript,
    draft: Signal<String>,
    running: bool,
    host_connected: bool,
    pump: Pump,
    on_field_mounted: EventHandler<Rc<MountedData>>,
) -> Element {
    let mut draft = draft;
    let disabled = !host_connected;
    let conversation_id = conversation.id.clone();
    let navigate = use_navigate();
    let mut selected_option = use_signal(|| 0_usize);
    let mut menu_dismissed = use_signal(|| false);
    let mut field = use_signal(|| None::<Rc<MountedData>>);
    let mut connect_provider = use_signal(|| None::<ProviderEntry>);
    let mut command_error = use_signal(|| None::<String>);
    let mut advisor_pending = use_signal(|| false);
    let report = report_command(command_error);
    let advisor_done = EventHandler::new(move |result: Result<(), String>| {
        advisor_pending.set(false);
        command_error.set(result.err());
    });
    let levels = effort_levels(&conversation, catalog.as_ref());
    let menu = slash_menu_state(&draft(), &levels);
    let option_count = menu.as_ref().map_or(0, |state| state.options.len());
    let menu_open = !menu_dismissed() && menu.is_some();
    let options_open = menu_open && option_count > 0;
    let selected = selected_option().min(option_count.saturating_sub(1));
    let selected_text = menu
        .as_ref()
        .and_then(|state| state.options.get(selected))
        .map(|option| option.text.clone());
    let listbox = listbox_id(&conversation_id);
    let send = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        let conversation = conversation.clone();
        let catalog = catalog.clone();
        move |()| {
            let text = draft.peek().trim().to_string();
            if let Some((command, args)) = parse_slash_command(&text) {
                if command.requires_argument() && !command.client_bare && args.is_empty() {
                    report.call(Err(format!("Usage: /{} {}", command.name, command.args)));
                    return;
                }
                if run_local_command(
                    command,
                    args,
                    &LocalCommandScope {
                        conversation: &conversation,
                        transcript: &transcript,
                        catalog: catalog.as_ref(),
                        pump: pump.clone(),
                        navigate,
                        connect_provider,
                    },
                ) {
                    draft.set(String::new());
                    return;
                }
            }
            submit_message(pump.clone(), conversation_id.clone(), text, report);
            draft.set(String::new());
        }
    });
    let accept = EventHandler::new({
        let levels = levels.clone();
        move |index: usize| {
            let current = draft.peek().clone();
            let Some(option) = slash_menu_state(&current, &levels)
                .and_then(|state| state.options.get(index).cloned())
            else {
                return;
            };
            if option.text.trim_end() == current.trim() {
                send.call(());
            } else {
                fill_draft(draft, selected_option, &option.text);
            }
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
        "Ask the agent, or type / for commands…"
    };
    let levels_for_input = levels.clone();
    let key_conversation = conversation_id.clone();
    let button_conversation = conversation_id.clone();
    rsx! {
        div {
            class: "agent-chat__composer",
            "data-disabled": disabled.then_some("true"),
            "data-testid": "agent-chat-composer",
            if let Some(state) = menu.clone().filter(|_| menu_open) {
                SlashMenu {
                    conversation_id: conversation_id.clone(),
                    state,
                    selected,
                    on_choose: accept,
                }
            }
            textarea {
                class: "agent-chat__input",
                "data-testid": "agent-chat-input",
                "aria-label": "Message the agent",
                role: "combobox",
                "aria-autocomplete": "list",
                "aria-expanded": if options_open { "true" } else { "false" },
                "aria-controls": options_open.then(|| listbox.clone()),
                "aria-activedescendant": options_open.then(|| option_id(&conversation_id, selected)),
                rows: "1",
                placeholder,
                disabled,
                value: "{draft}",
                onmounted: move |event: MountedEvent| {
                    let data = event.data();
                    field.set(Some(data.clone()));
                    on_field_mounted.call(data);
                },
                oninput: move |event: FormEvent| {
                    let value = event.value();
                    let next = slash_menu_state(&value, &levels_for_input);
                    selected_option.set(next.as_ref().map_or(0, |state| initial_selection(state, &value)));
                    draft.set(value);
                    menu_dismissed.set(false);
                },
                onkeydown: move |event: KeyboardEvent| {
                    let key = event.key().to_string();
                    if menu_open && key == "Escape" {
                        event.prevent_default();
                        menu_dismissed.set(true);
                        return;
                    }
                    if options_open {
                        match key.as_str() {
                            "ArrowDown" | "ArrowUp" => {
                                event.prevent_default();
                                let next = if key == "ArrowDown" {
                                    (selected + 1) % option_count
                                } else {
                                    selected.checked_sub(1).unwrap_or(option_count - 1)
                                };
                                selected_option.set(next);
                                scroll_option_into_view(&key_conversation, next);
                                return;
                            }
                            "Tab" => {
                                event.prevent_default();
                                if let Some(text) = &selected_text {
                                    fill_draft(draft, selected_option, text);
                                }
                                return;
                            }
                            _ => {}
                        }
                    }
                    if key == "Enter" && !event.modifiers().contains(Modifiers::SHIFT) && !event.is_composing() {
                        event.prevent_default();
                        if options_open {
                            accept.call(selected);
                        } else {
                            send.call(());
                        }
                    }
                },
            }
            if let Some(message) = command_error() {
                div { class: "agent-chat__notice", "data-tone": "error", role: "alert", "data-testid": "agent-command-error",
                    Icon { name: "error", size: IconSize::Sm }
                    span { "{message}" }
                }
            }
            div { class: "agent-chat__composer-bar",
                IconButton {
                    icon: "terminal",
                    label: "Show slash commands",
                    title: "Commands",
                    variant: ButtonVariant::Secondary,
                    size: IconButtonSize::Icon,
                    class: "agent-chat__commands",
                    "data-testid": "agent-chat-commands",
                    expanded: Some(options_open),
                    controls_id: Some(listbox.clone()),
                    onclick: move |_| {
                        if disabled {
                            return;
                        }
                        if draft.peek().is_empty() {
                            fill_draft(draft, selected_option, "/");
                        }
                        menu_dismissed.set(false);
                        focus_field(field);
                    },
                }
                if conversation.mode == "plan" {
                    Chip { label: "Plan mode".to_string(), icon: Some("assignment".to_string()), selected: Some(true),
                        onclick: Some(EventHandler::new({
                            let pump = pump.clone();
                            let conversation_id = button_conversation.clone();
                            move |_| submit_direct_command(pump.clone(), conversation_id.clone(), "/plan".to_string(), report)
                        }))
                    }
                }
                Chip { label: "Advisor".to_string(), icon: Some("psychology".to_string()), selected: Some(conversation.advisor),
                    title: Some(advisor_title(conversation.advisor, advisor_pending()).to_string()),
                    test_id: Some("agent-advisor-toggle".to_string()),
                    disabled: advisor_pending(),
                    busy: advisor_pending(),
                    onclick: Some(EventHandler::new({
                        let pump = pump.clone();
                        let conversation_id = button_conversation.clone();
                        move |_| {
                            if advisor_pending() {
                                return;
                            }
                            advisor_pending.set(true);
                            submit_direct_command(pump.clone(), conversation_id.clone(), "/advisor".to_string(), advisor_done);
                        }
                    }))
                }
                AgentModelControls { conversation: conversation.clone(), catalog, catalog_failed, pump: pump.clone() }
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

/// Replaces the draft with a completion and puts the highlight back on the first option.
fn fill_draft(mut draft: Signal<String>, mut selected_option: Signal<usize>, text: &str) {
    draft.set(text.to_string());
    selected_option.set(0);
}

fn focus_field(field: Signal<Option<Rc<MountedData>>>) {
    let Some(handle) = field.peek().clone() else {
        return;
    };
    spawn(async move {
        let _ = handle.set_focus(true).await;
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
