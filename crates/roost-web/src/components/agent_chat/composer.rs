//! The agent composer keeps the draft, slash commands and voice input together.
//! Its footer carries model, plan, voice and send; the status line beneath it carries
//! usage, advisor and goal. Submissions reuse their request ID until the outcome is known.
use super::composer_commands::report_command;
use super::composer_footer::ComposerFooter;
#[cfg(target_arch = "wasm32")]
use super::composer_helpers::abort_run;
use super::composer_helpers::{
    EditBanner, composer_placeholder, composer_run_facts, fill_draft, focus_field,
};
use super::composer_status::{ComposerErrorBanner, ComposerStatusLine, composer_usage};
use super::goal_controls::GoalControls;
use super::message_actions::EditTarget;
use super::slash_menu::{
    SlashAccept, SlashMenu, accept_action, initial_selection, listbox_id, option_id,
    scroll_option_into_view, slash_menu_state, slash_sources,
};
use crate::components::settings::agent_connect_dialog::AgentConnectDialog;
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use crate::components::terminal_chrome::attachment_picker::spawn_read_chosen;
use crate::components::terminal_chrome::composer_dictation::{
    SharedBinding, VoiceControl, use_dictation, use_dictation_context,
};
use crate::components::ui::{FieldError, InputPrompt, InputPromptTextarea};
use crate::pump::Pump;
use crate::router_state::use_navigate;
use crate::voice::send_gate::SendGate;
use dioxus::prelude::*;
use dioxus::web::WebEventExt as _;
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, ProviderEntry, Transcript,
};
use std::rc::Rc;
#[component]
pub fn AgentComposer(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    catalog_failed: bool,
    transcript: Transcript,
    draft: Signal<String>,
    edit_target: Signal<Option<EditTarget>>,
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
    let mut voice_error = use_signal(|| None::<String>);
    let mut goal_open = use_signal(|| false);
    let goal_view_request = use_signal(String::new);
    let mut goal_pending = use_signal(|| false);
    let unresolved_send = use_signal(|| super::submission::restore(&conversation_id));
    let busy_mode = use_signal(|| {
        unresolved_send
            .peek()
            .as_ref()
            .map(|pending| pending.when_busy.clone())
            .unwrap_or_else(|| "follow_up".to_owned())
    });
    let submit_pending = use_signal(|| false);
    let staged_files = use_signal(Vec::<ChosenFile>::new);
    let goal_result = EventHandler::new(move |result: Result<(), String>| {
        goal_pending.set(false);
        command_error.set(result.err());
    });
    let report = report_command(command_error);
    let sources = slash_sources(&conversation, catalog.as_ref());
    let menu = if edit_target.peek().is_some() {
        None
    } else {
        slash_menu_state(&draft(), &sources)
    };
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
    let (goal, failed, cost_usd) = composer_run_facts(&transcript);
    let usage = composer_usage(&transcript, catalog.as_ref(), &conversation);
    let submit =
        super::composer_submit::submit_handler(super::composer_submit::ComposerSubmitContext {
            conversation: conversation.clone(),
            catalog: catalog.clone(),
            transcript,
            draft,
            edit_target,
            disabled,
            pump: pump.clone(),
            goal_open,
            goal_view_request,
            connect_provider,
            model_hub_open,
            navigate,
            report,
            unresolved_send,
            busy_mode,
            submit_pending,
            staged_files,
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
    let uploads_in_flight = staged_files()
        .iter()
        .any(|file| file.upload_state.is_in_flight());
    let attachments_block_send = staged_files()
        .iter()
        .any(|file| file.upload_state.blocks_send());
    let on_attach = super::composer_attachments::checked_stage_attachments(
        pump.clone(),
        staged_files,
        conversation.clone(),
        edit_target,
        draft,
        report,
    );
    let on_drop = EventHandler::new(move |event: DragEvent| {
        event.prevent_default();
        let files = super::composer_attachments::transferred_files(
            event
                .try_as_web_event()
                .and_then(|native: web_sys::DragEvent| native.data_transfer())
                .and_then(|transfer| transfer.files()),
        );
        spawn_read_chosen(files, on_attach);
    });
    let on_paste = EventHandler::new(move |event: ClipboardEvent| {
        let files = super::composer_attachments::transferred_files(
            event
                .try_as_web_event()
                .and_then(|native: web_sys::ClipboardEvent| native.clipboard_data())
                .and_then(|transfer| transfer.files()),
        );
        if !files.is_empty() {
            event.prevent_default();
            spawn_read_chosen(files, on_attach);
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
    let dictation_finishing = dictation.send_gate() == SendGate::AfterFinalize;
    let send_available = dictation.send_gate().offers_send();
    let placeholder = composer_placeholder(disabled, send_available, running, &busy_mode());
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
        if !menu_open && key == "Escape" && edit_target.peek().is_some() {
            event.prevent_default();
            edit_target.set(None);
            draft.set(String::new());
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
            ondragover: move |event: DragEvent| event.prevent_default(),
            ondrop: on_drop,
            onpaste: on_paste,
            if let Some(state) = menu.clone().filter(|_| menu_open) {
                SlashMenu {
                    conversation_id: conversation_id.clone(),
                    state,
                    selected,
                    on_choose: pick,
                }
            }
            EditBanner { edit_target, draft }
            InputPrompt {
                class: "min-w-0 flex-col",
                InputPromptTextarea {
                    id: super::composer_helpers::composer_input_id(&conversation_id),
                    value: draft,
                    placeholder,
                    aria_label: "Message the agent",
                    disabled,
                    submit_on_enter: send_available,
                    class: "w-full",
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
                    has_draft: !empty
                        || !staged_files().is_empty()
                        || edit_target
                            .peek()
                            .as_ref()
                            .is_some_and(EditTarget::has_attachments),
                    send_available,
                    dictation_finishing,
                    voice_controls: rsx! {
                        VoiceControl {
                            owner_id: conversation_id.clone(),
                            active: active && !disabled,
                            read_context: Some(dictation_context.clone()),
                            binding: SharedBinding(dictation.clone()),
                            on_submit: submit,
                            on_error: move |error| voice_error.set(error),
                        }
                    },
                    staged_files,
                    on_chosen: on_attach,
                    uploads_in_flight,
                    attachments_block_send,
                    disabled,
                    busy_mode,
                    submit_pending: submit_pending(),
                    on_send: send,
                    on_stop: stop,
                }
            }
            ComposerStatusLine {
                conversation: conversation.clone(),
                usage,
                cost_usd,
                running,
                failed,
            }
            ComposerErrorBanner {
                error: run_error,
                running,
                on_stop: stop,
                on_switch_model: switch_model,
            }
            if let Some(message) = command_error() {
                FieldError { test_id: "agent-command-error", class: "px-3", {message} }
            }
            if let Some(message) = voice_error() {
                FieldError { test_id: "agent-voice-error", class: "px-3", {message} }
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
