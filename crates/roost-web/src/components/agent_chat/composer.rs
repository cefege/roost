//! The agent composer keeps the draft, slash commands and voice input together.
//! Its footer exposes models, busy submission modes and foreground interruption.
//! Submissions reuse their request ID until the outcome is known.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, ProviderEntry, Transcript, parse_slash_command,
};

use super::composer_footer::ComposerFooter;
#[cfg(target_arch = "wasm32")]
use super::composer_helpers::abort_run;
use super::composer_helpers::{fill_draft, focus_field};
use super::composer_status::ComposerErrorBanner;
use super::goal_controls::GoalControls;
use super::slash_menu::{
    SlashAccept, SlashMenu, accept_action, initial_selection, listbox_id, option_id,
    scroll_option_into_view, slash_menu_state, slash_sources,
};
use crate::components::settings::agent_connect_dialog::AgentConnectDialog;
use crate::components::terminal_chrome::composer_dictation::{
    SharedBinding, VoiceControl, use_dictation, use_dictation_context,
};
use crate::components::ui::{FieldError, InputPrompt, InputPromptTextarea};
use crate::pump::Pump;
use crate::router_state::use_navigate;
use crate::voice::send_gate::SendGate;

use super::composer_commands::{
    LocalCommandScope, report_command, run_local_command, submit_message,
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
    active: bool,
    pump: Pump,
    model_hub_open: Signal<bool>,
    connect_provider: Signal<Option<ProviderEntry>>,
) -> Element {
    let mut draft = draft;
    let dictation = use_dictation(draft);
    let dictation_context = use_dictation_context(None, draft);
    let disabled = !host_connected;
    let conversation_id = conversation.id.clone();
    let navigate = use_navigate();
    let mut selected_option = use_signal(|| 0_usize);
    let mut menu_dismissed = use_signal(|| false);
    let mut field = use_signal(|| None::<Rc<MountedData>>);
    let mut connect_provider = connect_provider;
    let mut command_error = use_signal(|| None::<String>);
    let mut goal_open = use_signal(|| false);
    let mut goal_view_request = use_signal(String::new);
    let mut goal_pending = use_signal(|| false);
    let mut unresolved_send = use_signal(|| super::submission::restore(&conversation_id));
    let busy_mode = use_signal(|| {
        unresolved_send
            .peek()
            .as_ref()
            .map(|pending| pending.when_busy.clone())
            .unwrap_or_else(|| "follow_up".to_owned())
    });
    let mut submit_pending = use_signal(|| false);
    let goal_result = EventHandler::new(move |result: Result<(), String>| {
        goal_pending.set(false);
        command_error.set(result.err());
    });
    let report = report_command(command_error);
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
    let submit = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        let conversation = conversation.clone();
        let catalog = catalog.clone();
        move |()| {
            if disabled || submit_pending() {
                return;
            }
            let text = draft.peek().trim().to_string();
            if text.is_empty() {
                return;
            }
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
                        report,
                    },
                ) {
                    draft.set(String::new());
                    return;
                }
            }
            let selected_mode = busy_mode.peek().clone();
            let pending = super::submission::prepare(
                &conversation_id,
                &text,
                &selected_mode,
                unresolved_send.peek().as_ref(),
            );
            let (mode, request_id) = (pending.when_busy.clone(), pending.request_id.clone());
            unresolved_send.set(Some(pending));
            submit_pending.set(true);
            let completion = super::submission::completion_callback(
                conversation_id.clone(),
                draft,
                submit_pending,
                unresolved_send,
                report,
            );
            submit_message(
                pump.clone(),
                conversation_id.clone(),
                text,
                mode,
                request_id,
                completion,
            );
        }
    });
    let send = EventHandler::new({
        let dictation = dictation.clone();
        move |()| match dictation.send_gate() {
            SendGate::Submit => submit.call(()),
            SendGate::AfterFinalize => dictation.send_after_finalize(),
            SendGate::Withheld => {}
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
            match accept_action(&option) {
                SlashAccept::Fill(text) => {
                    fill_draft(draft, selected_option, &text);
                    // A tap moved focus to the row; the argument is typed next.
                    focus_field(field);
                }
                SlashAccept::Submit(text) => {
                    draft.set(text);
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
    let send_available = dictation.send_gate().offers_send();
    let placeholder = if disabled {
        "The agent host is offline"
    } else if !send_available {
        "Listening…"
    } else if running {
        match busy_mode().as_str() {
            "follow_up" => "Add a follow-up…",
            "reject" => "This message will be rejected while busy",
            _ => "Steer the agent…",
        }
    } else {
        "Ask the agent, or type / for commands…"
    };
    let sources_for_input = sources.clone();
    let key_conversation = conversation_id.clone();
    // A tap or click on a row does what Enter does on it: runs a command that
    // needs nothing more, completes one that needs an argument.
    let pick = accept;
    let capture_field = EventHandler::new(move |event: MountedEvent| field.set(Some(event.data())));
    let input_dictation = dictation.clone();
    let handle_draft_input = EventHandler::new(move |value: String| {
        input_dictation.forget_provisional();
        let next = slash_menu_state(&value, &sources_for_input);
        selected_option.set(
            next.as_ref()
                .map_or(0, |state| initial_selection(state, &value)),
        );
        menu_dismissed.set(false);
    });
    let handle_draft_key = EventHandler::new(move |event: KeyboardEvent| {
        let key = event.key().to_string();
        if menu_open && key == "Escape" {
            event.prevent_default();
            menu_dismissed.set(true);
            return;
        }
        if running && key == "Escape" {
            event.prevent_default();
            event.stop_propagation();
            stop.call(());
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
                }
                "Tab" => {
                    event.prevent_default();
                    if let Some(text) = &selected_text {
                        fill_draft(draft, selected_option, text);
                    }
                }
                _ => {}
            }
        }
    });
    let submit_draft = EventHandler::new(move |()| {
        if options_open {
            accept.call(selected);
        } else {
            send.call(());
        }
    });
    rsx! {
        div {
            class: "relative flex flex-col gap-1 data-disabled:opacity-60",
            "data-disabled": disabled.then_some("true"),
            "data-testid": "agent-chat-composer",
            if let Some(state) = menu.clone().filter(|_| menu_open) {
                SlashMenu {
                    conversation_id: conversation_id.clone(),
                    state,
                    selected,
                    on_choose: pick,
                }
            }
            InputPrompt {
                class: "min-w-0 flex-col",
                ComposerErrorBanner {
                    error: run_error,
                    running,
                    on_stop: stop,
                    on_switch_model: switch_model,
                }
                InputPromptTextarea {
                    id: super::composer_helpers::composer_input_id(&conversation_id),
                    value: draft,
                    placeholder,
                    aria_label: "Message the agent",
                    disabled,
                    submit_on_enter: send_available,
                    class: "w-full min-h-6 max-h-60 py-1 px-3 text-base leading-6",
                    on_mounted: capture_field,
                    on_keydown: handle_draft_key,
                    on_input: handle_draft_input,
                    on_submit: submit_draft,
                    role: "combobox",
                    "aria-autocomplete": "list",
                    "aria-expanded": if options_open { "true" } else { "false" },
                    "aria-controls": options_open.then(|| listbox.clone()),
                    "aria-activedescendant": options_open.then(|| option_id(&conversation_id, selected)),
                    rows: "1",
                    "data-testid": "agent-chat-input",
                }
                if let Some(message) = command_error() {
                    FieldError { test_id: "agent-command-error", class: "px-3 pb-1", {message} }
                }
                ComposerFooter {
                    conversation: conversation.clone(),
                    catalog,
                    catalog_failed,
                    pump: pump.clone(),
                    goal: goal.clone(),
                    goal_view_request,
                    on_goal_open: move |_| goal_open.set(true),
                    command_error,
                    commands_open: options_open,
                    listbox: listbox.clone(),
                    on_commands: move |_| {
                        if disabled {
                            return;
                        }
                        if draft.peek().is_empty() {
                            fill_draft(draft, selected_option, "/");
                        }
                        menu_dismissed.set(false);
                        focus_field(field);
                    },
                    running,
                    has_draft: !empty,
                    send_available,
                    voice_controls: rsx! {
                        VoiceControl {
                            owner_id: conversation_id.clone(),
                            active: active && !disabled,
                            compact: true,
                            read_context: Some(dictation_context.clone()),
                            binding: SharedBinding(dictation.clone()),
                            on_submit: submit,
                        }
                    },
                    disabled,
                    busy_mode,
                    submit_pending: submit_pending(),
                    on_send: send,
                    on_stop: stop,
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
