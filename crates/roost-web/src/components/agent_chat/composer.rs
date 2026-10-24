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
use super::composer_options::{render_slash_option, render_slash_usage};
use super::composer_status::ComposerErrorBanner;
use super::goal_controls::GoalControls;
use super::slash_menu::{SlashAccept, accept_action, slash_menu_state, slash_sources};
use crate::components::settings::agent_connect_dialog::AgentConnectDialog;
use crate::components::terminal_chrome::composer_dictation::{
    SharedBinding, VoiceControl, use_dictation, use_dictation_context,
};
use crate::components::ui::{InputPrompt, InputPromptTextarea};
use crate::pump::Pump;
use crate::router_state::use_navigate;
use crate::voice::send_gate::SendGate;
use libero::components::{Combobox, Flex, Text, use_combobox};
use libero::sx::sx;

use super::composer_commands::{
    LocalCommandScope, report_command, run_local_command, submit_message,
};
#[path = "composer_input.rs"]
mod composer_input;

use composer_input::{composer_draft_handlers, composer_placeholder};

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
    let combobox = use_combobox();
    let composer_sx = use_hook(|| {
        sx().min_width("0")
            .display("flex")
            .flex_direction("column")
            .gap("xs")
            .selector("&[data-disabled=true]", sx().opacity("0.6"))
    });
    let textarea_sx = use_hook(|| sx().width("100%").max_height("15rem"));
    let textarea_control_sx = use_hook(|| {
        sx().padding_block("xs")
            .padding_inline("sm")
            .font_size("md")
    });
    let input_prompt_sx = use_hook(|| sx().min_width("0").display("flex").flex_direction("column"));
    let mut menu_dismissed = use_signal(|| false);
    let field = use_signal(|| None::<Rc<MountedData>>);
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
    let selected = combobox
        .active()
        .unwrap_or(0)
        .min(option_count.saturating_sub(1));
    let selected_text = menu
        .as_ref()
        .and_then(|state| state.options.get(selected))
        .map(|option| option.text.clone());
    let listbox = combobox.id();
    let options = menu
        .as_ref()
        .filter(|_| options_open)
        .map(|state| state.options.clone())
        .unwrap_or_default();
    let usage_line = menu
        .as_ref()
        .filter(|state| menu_open && state.options.is_empty())
        .and_then(|state| state.argument_command);
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
                    fill_draft(draft, &text);
                    combobox.open();
                    // A tap moved focus to the row; the argument is typed next.
                    focus_field(field);
                }
                SlashAccept::Submit(text) => {
                    draft.set(text);
                    combobox.close();
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
    let placeholder = composer_placeholder(disabled, send_available, running, &busy_mode());
    let input_dictation = dictation.clone();
    let on_provisional_input = EventHandler::new(move |()| input_dictation.forget_provisional());
    let input_handlers = composer_draft_handlers(
        draft,
        sources.clone(),
        field,
        combobox,
        menu_dismissed,
        menu_open,
        options_open,
        selected_text,
        running,
        send_available,
        on_provisional_input,
        stop,
        send,
    );
    let effect_sources = sources.clone();
    use_effect(move || {
        let current_draft = draft();
        if !menu_dismissed() && slash_menu_state(&current_draft, &effect_sources).is_some() {
            combobox.open();
        } else {
            combobox.close();
        }
    });
    rsx! {
        Combobox {
            state: combobox,
            disabled: disabled || !menu_open,
            options,
            option: move |row| render_slash_option(row, accept),
            empty: usage_line.map(|command| render_slash_usage(command.name, command.args, command.summary)),
            Flex {
                direction: "column",
                gap: "xs",
                sx: composer_sx,
                "data-disabled": disabled.then_some("true"),
                "data-testid": "agent-chat-composer",
                InputPrompt {
                    sx: input_prompt_sx,
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
                        rows: 1,
                        sx: textarea_sx,
                        control_sx: textarea_control_sx,
                        on_mounted: input_handlers.mounted,
                        on_keydown: input_handlers.keydown,
                        on_input: input_handlers.input,
                        on_submit: input_handlers.submit,
                        attributes: combobox.a11y_attributes(),
                        onblur: move |_| combobox.close(),
                        onfocus: move |_| if menu_open { combobox.set_active(Some(selected)); combobox.open(); },
                        "data-testid": "agent-chat-input",
                    }
                    if let Some(message) = command_error() {
                        Text { color: "error", role: "alert", "data-testid": "agent-command-error", "{message}" }
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
                    commands_open: options_open && !disabled && combobox.is_open(),
                    listbox: listbox.clone(),
                    on_commands: move |_| {
                        if disabled {
                            return;
                        }
                        if draft.peek().is_empty() {
                            fill_draft(draft, "/");
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
