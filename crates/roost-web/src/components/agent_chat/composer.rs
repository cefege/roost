//! HTML draft entry, slash completion and live agent controls.
//! The surface keeps the draft; the pump submits input and explicit Stop.
//! Model, settings and goal actions remain clickable while output streams.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, ProviderEntry, Transcript, parse_slash_command,
};

use super::agent_alert::ERROR_TEXT_CLASS;
#[cfg(target_arch = "wasm32")]
use super::composer_helpers::abort_run;
use super::composer_helpers::{fill_draft, focus_field};
use super::composer_status::ComposerErrorBanner;
use super::goal_controls::GoalControls;
use super::goal_dock::GoalDock;
use super::model_controls::AgentModelControls;
use super::slash_menu::{
    SlashMenu, initial_selection, listbox_id, option_id, scroll_option_into_view, slash_menu_state,
    slash_sources,
};
use crate::components::md::{ButtonVariant, Chip, IconButton, IconButtonSize, Surface};
use crate::components::settings::agent_connect_dialog::AgentConnectDialog;
use crate::pump::Pump;
use crate::router_state::use_navigate;

use super::composer_commands::{
    LocalCommandScope, advisor_title, report_command, run_local_command, submit_direct_command,
    submit_message,
};

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
    model_hub_open: Signal<bool>,
    connect_provider: Signal<Option<ProviderEntry>>,
) -> Element {
    let mut draft = draft;
    let disabled = !host_connected;
    let conversation_id = conversation.id.clone();
    let navigate = use_navigate();
    let mut selected_option = use_signal(|| 0_usize);
    let mut menu_dismissed = use_signal(|| false);
    let mut field = use_signal(|| None::<Rc<MountedData>>);
    let mut connect_provider = connect_provider;
    let mut command_error = use_signal(|| None::<String>);
    let mut advisor_pending = use_signal(|| false);
    let mut goal_open = use_signal(|| false);
    let mut goal_view_request = use_signal(String::new);
    let mut goal_pending = use_signal(|| false);
    let goal_result = EventHandler::new(move |result: Result<(), String>| {
        goal_pending.set(false);
        command_error.set(result.err());
    });
    let report = report_command(command_error);
    let advisor_done = EventHandler::new(move |result: Result<(), String>| {
        advisor_pending.set(false);
        command_error.set(result.err());
    });
    let sources = slash_sources(&conversation, catalog.as_ref());
    let menu = slash_menu_state(&draft(), &sources);
    let option_count = menu.as_ref().map_or(0, |state| state.options.len());
    let menu_open = !menu_dismissed() && menu.is_some();
    let options_open = menu_open && option_count > 0;
    let selected = selected_option().min(option_count.saturating_sub(1));
    let selected_text = menu
        .as_ref()
        .and_then(|state| state.options.get(selected))
        .map(|option| option.text.clone());
    let listbox = listbox_id(&conversation_id);
    let run_error = transcript.error.clone();
    let goal = transcript.items.iter().rev().find_map(|item| match item {
        roost_protocol::wire::agent_chat::TranscriptItem::Goal { state, .. } => Some(state.clone()),
        _ => None,
    });
    let usage = format!(
        "{} tokens · ${:.4}",
        transcript.usage.input_tokens + transcript.usage.output_tokens,
        transcript.usage.cost_usd
    );
    let send = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        let conversation = conversation.clone();
        let catalog = catalog.clone();
        move |()| {
            let text = draft.peek().trim().to_string();
            if let Some((command, args)) = parse_slash_command(&text) {
                if command.name == "goal"
                    && (args.is_empty()
                        || args.trim().eq_ignore_ascii_case("set")
                        || args.trim().eq_ignore_ascii_case("budget"))
                {
                    goal_view_request.set(args.trim().to_ascii_lowercase());
                    goal_open.set(true);
                    draft.set(String::new());
                    return;
                }
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
                        model_hub_open,
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
        let sources = sources.clone();
        move |index: usize| {
            let current = draft.peek().clone();
            let Some(option) = slash_menu_state(&current, &sources)
                .and_then(|state| state.options.get(index).cloned())
            else {
                return;
            };
            if option.text.trim_end() == current.trim() {
                send.call(());
            } else {
                fill_draft(draft, selected_option, &option.text);
                if (option.command.client_bare
                    && option.text.trim_end().strip_prefix('/') == Some(option.command.name))
                    || (option.command.name == "goal" && option.text.trim_end() == "/goal")
                {
                    send.call(());
                }
            }
        }
    });
    let stop = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        move |()| {
            #[cfg(target_arch = "wasm32")]
            abort_run(pump.clone(), conversation_id.clone());
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (&pump, &conversation_id);
        }
    });
    let switch_model = EventHandler::new({
        let conversation_id = conversation_id.clone();
        move |()| {
            #[cfg(target_arch = "wasm32")]
            super::composer_commands::open_model_picker(&conversation_id);
            #[cfg(not(target_arch = "wasm32"))]
            let _ = &conversation_id;
        }
    });
    let empty = draft().trim().is_empty();
    let placeholder = if disabled {
        "The agent host is offline"
    } else if running {
        "Steer the agent…"
    } else {
        "Ask the agent, or type / for commands…"
    };
    let sources_for_input = sources.clone();
    let key_conversation = conversation_id.clone();
    let button_conversation = conversation_id.clone();
    rsx! {
        div {
            class: "agent-chat__composer",
            "data-disabled": disabled.then_some("true"),
            "data-testid": "agent-chat-composer",
            Surface { class: "agent-chat__composer-panel", level: 1, elevation: 1, border: true, pad: Some(3),
                ComposerErrorBanner {
                    error: run_error,
                    running,
                    on_stop: stop,
                    on_switch_model: switch_model,
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
                        let next = slash_menu_state(&value, &sources_for_input);
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
                if let Some(state) = menu.clone().filter(|_| menu_open) {
                    SlashMenu {
                        conversation_id: conversation_id.clone(),
                        state,
                        selected,
                        on_choose: accept,
                    }
                }
                if let Some(message) = command_error() {
                    p { class: ERROR_TEXT_CLASS, role: "alert", "data-testid": "agent-command-error", "{message}" }
                }
                div { class: "agent-chat__composer-bar",
                    AgentModelControls { conversation: conversation.clone(), catalog, catalog_failed, pump: pump.clone() }
                    span {
                        class: "agent-chat__usage",
                        "data-testid": "agent-chat-usage",
                        title: "Session token usage and cost",
                        "{usage}"
                    }
                    span { class: "agent-chat__status", "data-testid": "agent-chat-status",
                        if disabled { "Offline" } else if running { "Running" } else { "Ready" }
                    }
                    IconButton {
                        icon: "apps",
                        label: "Show agent commands",
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
                    GoalDock {
                        goal: goal.clone(),
                        view_request: goal_view_request,
                        on_open: move |_| goal_open.set(true),
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
                    IconButton {
                        icon: "settings",
                        label: "Agent settings",
                        title: "Models, accounts and agent settings",
                        variant: ButtonVariant::Ghost,
                        size: IconButtonSize::Icon,
                        "data-testid": "agent-chat-settings",
                        onclick: move |_| navigate.call(crate::routes::settings_pane_href("models")),
                    }
                    span { class: "agent-chat__composer-spacer" }
                    if running {
                        IconButton {
                            icon: "stop",
                            label: "Stop the agent",
                            title: "Stop",
                            variant: ButtonVariant::Secondary,
                            size: IconButtonSize::Icon,
                            class: "agent-chat__action",
                            "data-testid": "agent-chat-stop",
                            onclick: move |_| stop.call(()),
                        }
                    }
                    if !running || !empty {
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
            GoalControls {
                goal,
                open: goal_open,
                view_request: goal_view_request,
                pending: goal_pending,
                command_error,
                on_result: goal_result,
                conversation_id: conversation_id.clone(),
                pump: pump.clone(),
                disabled,
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
