//! Agent message submission keeps identity through retries and owns attachment handoff.
//! The composer supplies live state; successful sends clear only their submitted files.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, ProviderEntry, Transcript, parse_slash_command,
};

#[cfg(target_arch = "wasm32")]
use super::composer_attachments::submit_attachments;
use super::composer_commands::{LocalCommandScope, run_local_command, submit_message};
use super::composer_helpers::try_submit_edit;
use super::message_actions::EditTarget;
use super::submission::{PendingSubmission, completion_callback, prepare};
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use crate::pump::Pump;

pub(super) struct ComposerSubmitContext {
    pub conversation: ConversationSummary,
    pub catalog: Option<ModelsCatalog>,
    pub transcript: Transcript,
    pub draft: Signal<String>,
    pub edit_target: Signal<Option<EditTarget>>,
    pub disabled: bool,
    pub pump: Pump,
    pub goal_open: Signal<bool>,
    pub goal_view_request: Signal<String>,
    pub connect_provider: Signal<Option<ProviderEntry>>,
    pub model_hub_open: Signal<bool>,
    pub navigate: EventHandler<String>,
    pub report: EventHandler<Result<(), String>>,
    pub unresolved_send: Signal<Option<PendingSubmission>>,
    pub busy_mode: Signal<String>,
    pub submit_pending: Signal<bool>,
    pub staged_files: Signal<Vec<ChosenFile>>,
}

pub(super) fn submit_handler(context: ComposerSubmitContext) -> EventHandler<()> {
    let ComposerSubmitContext {
        conversation,
        catalog,
        transcript,
        mut draft,
        edit_target,
        disabled,
        pump,
        mut goal_open,
        mut goal_view_request,
        connect_provider,
        model_hub_open,
        navigate,
        report,
        mut unresolved_send,
        busy_mode,
        mut submit_pending,
        staged_files,
    } = context;
    let conversation_id = conversation.id.clone();
    EventHandler::new({
        let pump = pump.clone();
        let conversation = conversation.clone();
        let catalog = catalog.clone();
        move |()| {
            if disabled || submit_pending() {
                return;
            }
            let text = draft.peek().trim().to_string();
            let attachments = staged_files.peek().clone();
            if attachments
                .iter()
                .any(|file| file.upload_state.blocks_send())
            {
                return;
            }
            let target = edit_target.peek();
            let restored = matches!(target.as_ref(), Some(EditTarget::Rewound { .. }));
            let original_attachments = target
                .as_ref()
                .map(EditTarget::retained_attachments)
                .unwrap_or_default();
            if text.is_empty()
                && attachments.is_empty()
                && !target.as_ref().is_some_and(EditTarget::has_attachments)
            {
                return;
            }
            if matches!(target.as_ref(), Some(EditTarget::PendingRewind { .. })) {
                submit_pending.set(true);
                let edit_report = EventHandler::new(move |result: Result<(), String>| {
                    submit_pending.set(false);
                    report.call(result);
                });
                if try_submit_edit(
                    edit_target,
                    draft,
                    &pump,
                    &conversation_id,
                    &text,
                    staged_files,
                    edit_report,
                ) {
                    return;
                }
                submit_pending.set(false);
            }
            if !restored
                && !text.is_empty()
                && let Some((command, args)) = parse_slash_command(&text)
            {
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
            let original_attachments = original_attachments.to_vec();
            let mut attachment_ids = target
                .as_ref()
                .and_then(|target| match target {
                    EditTarget::Rewound {
                        item_id,
                        attachments,
                    } => Some(
                        attachments
                            .iter()
                            .enumerate()
                            .map(|(idx, _)| format!("journal:{item_id}:{idx}"))
                            .collect::<Vec<_>>(),
                    ),
                    EditTarget::PendingRewind { .. } => None,
                })
                .unwrap_or_default();
            attachment_ids.extend(attachments.iter().filter_map(|file| file.upload_id.clone()));
            let restored_origin = match target.as_ref() {
                Some(EditTarget::Rewound { item_id, .. }) => Some(item_id.clone()),
                _ => None,
            };
            let mut target_on_success = edit_target;
            let selected_mode = busy_mode.peek().clone();
            let pending = prepare(
                &conversation_id,
                &text,
                &selected_mode,
                &attachment_ids,
                unresolved_send.peek().as_ref(),
            );
            let (mode, request_id) = (pending.when_busy.clone(), pending.request_id.clone());
            unresolved_send.set(Some(pending));
            submit_pending.set(true);
            let outcome = completion_callback(
                conversation_id.clone(),
                draft,
                submit_pending,
                unresolved_send,
                report,
            );
            let completion = EventHandler::new(move |result: Result<(), String>| {
                if result.is_ok() {
                    super::composer_attachments::clear_submitted_files(
                        staged_files,
                        &attachment_ids,
                    );
                    if let Some(origin) = restored_origin.as_deref()
                        && let Ok(mut target) = target_on_success.try_write()
                        && matches!(target.as_ref(), Some(EditTarget::Rewound { item_id, .. }) if item_id == origin)
                    {
                        *target = None;
                    }
                }
                outcome.call(result);
            });
            #[cfg(target_arch = "wasm32")]
            {
                let pump = pump.clone();
                let conversation_id = conversation_id.clone();
                spawn(async move {
                    match submit_attachments(&attachments).await {
                        Ok(mut new_attachments) => {
                            let mut all_attachments = Vec::with_capacity(
                                original_attachments.len() + new_attachments.len(),
                            );
                            all_attachments.extend(original_attachments);
                            all_attachments.append(&mut new_attachments);
                            if let Err(error) =
                                roost_protocol::wire::agent_chat::validate_agent_chat_image_payload(
                                    &text,
                                    &all_attachments,
                                )
                            {
                                completion.call(Err(error.to_string()));
                                return;
                            }
                            submit_message(
                                pump,
                                conversation_id,
                                text,
                                mode,
                                request_id,
                                all_attachments,
                                completion,
                            );
                        }
                        Err(error) => completion.call(Err(error)),
                    }
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            if attachments.is_empty() && original_attachments.is_empty() {
                submit_message(
                    pump.clone(),
                    conversation_id.clone(),
                    text,
                    mode,
                    request_id,
                    Vec::new(),
                    completion,
                );
            } else {
                completion.call(Err(
                    "File attachments are available in the web client.".to_owned()
                ));
            }
        }
    })
}
