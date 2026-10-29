//! The agent composer keeps the draft, slash commands and voice input together.
//! Its footer carries model, plan, voice and send; the status line beneath it carries
//! usage, advisor and goal. Submissions reuse their request ID until the outcome is known.
use super::composer_attachments::{
    AttachmentGate, AttachmentReports, ComposerAttachmentHandlers, composer_attachment_handlers,
    use_attachment_feedback,
};
use super::composer_commands::report_command;
use super::composer_footer::ComposerFooter;
use super::composer_helpers::{
    DraftKeyContext, EditBanner, composer_placeholder, composer_run_facts, draft_key_handler,
    fill_draft, focus_field, model_picker_handler, stop_handler,
};
use super::composer_status::{ComposerErrorBanner, ComposerStatusLine, composer_usage};
use super::goal_controls::GoalControls;
use super::message_actions::EditTarget;
use super::plan_refinement::{
    PlanDecisionBanner, PlanRefinement, PlanRefinementBanner, pending_proposal_id,
};
use super::slash_menu::{
    SlashAccept, SlashMenu, accept_action, initial_selection, listbox_id, option_id,
    slash_menu_state, slash_sources,
};
use crate::components::settings::agent_connect_dialog::AgentConnectDialog;
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use crate::components::terminal_chrome::composer_dictation::{
    SharedBinding, VoiceControl, use_dictation, use_dictation_context,
};
use crate::components::ui::{FieldError, InputPrompt, InputPromptTextarea};
use crate::pump::Pump;
use crate::router_state::use_navigate;
use crate::voice::send_gate::SendGate;
use dioxus::prelude::*;
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
    let plan_refinement = use_context::<PlanRefinement>();
    let plan_target = plan_refinement.selected_target();
    let plan_is_current = plan_refinement.is_current(&transcript);
    let pending_plan = pending_proposal_id(&transcript).map(str::to_owned);
    let pending_plan_decision = pending_plan.is_some();
    let refining_plan = plan_refinement.is_active();
    let composer_locked = pending_plan_decision && !refining_plan;
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
    use_effect(move || {
        if plan_refinement.focus_request().is_some() {
            focus_field(field);
        }
    });
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
    let menu = if composer_locked || edit_target.peek().is_some() || plan_target.is_some() {
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
            plan_refinement,
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
    let attachment_gate = AttachmentGate::for_plan_state(pending_plan_decision, refining_plan);
    let mut attachment_error = use_attachment_feedback(attachment_gate);
    let attachment_report =
        EventHandler::new(move |result: Result<(), String>| attachment_error.set(result.err()));
    let ComposerAttachmentHandlers {
        uploads_in_flight,
        blocks_send: attachments_block_send,
        on_chosen: on_attach,
        on_drop,
        on_paste,
    } = composer_attachment_handlers(
        pump.clone(),
        staged_files,
        conversation.clone(),
        edit_target,
        draft,
        AttachmentReports {
            ordinary: report,
            gated: attachment_report,
        },
        attachment_gate,
    );
    let stop = stop_handler(pump.clone(), conversation_id.clone());
    let switch_model = model_picker_handler(conversation_id.clone());
    let refinement_has_attachments =
        plan_refinement.attachments_block_send(staged_files, edit_target);
    let empty = draft().trim().is_empty();
    let dictation_finishing = dictation.send_gate() == SendGate::AfterFinalize;
    let send_available = dictation.send_gate().offers_send();
    let sources_for_input = sources.clone();
    let placeholder = if plan_target.is_some() {
        "Describe the changes you want…"
    } else {
        composer_placeholder(disabled, send_available, running, &busy_mode())
    };
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
    let handle_draft_key = draft_key_handler(DraftKeyContext {
        menu_open,
        menu_dismissed,
        edit_target,
        draft,
        running,
        stop,
        options_open,
        selected,
        option_count,
        selected_option,
        conversation_id: conversation_id.clone(),
        selected_text,
    });
    let submit_draft = EventHandler::new(move |()| match options_open {
        true => accept.call(selected),
        false => send.call(()),
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
            if let Some(item_id) = pending_plan.filter(|_| composer_locked) {
                PlanDecisionBanner { item_id }
            }
            if let Some(target) = plan_target {
                PlanRefinementBanner {
                    refinement: plan_refinement,
                    target,
                    attachments_block_send: refinement_has_attachments,
                    valid: plan_is_current,
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
                    readonly: composer_locked,
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
                        if disabled || composer_locked {
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
                            active: active && !disabled && !composer_locked,
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
                    attachments_disabled: attachment_gate.is_blocked(),
                    refining_plan,
                    plan_decision_pending: pending_plan_decision,
                    refinement_has_attachments,
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
            if let Some(message) = command_error().or_else(&*attachment_error) {
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
