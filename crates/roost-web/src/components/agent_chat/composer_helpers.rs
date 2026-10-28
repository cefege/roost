//! Shared focus, draft and stop mechanics for the composer.
//! These helpers keep the mounted input and the harness abort path consistent.
//! Called by the composer and workspace starter actions.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentRunState, GoalState, Transcript, TranscriptItem, parse_slash_command,
};

use crate::pump::Pump;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::AbortAgentChat;

use super::message_actions::EditTarget;

pub(super) fn composer_input_id(conversation_id: &str) -> String {
    format!("agent-chat-input-{conversation_id}")
}

/// The input placeholder for the current composer state, in the wording the composer
/// has always used; `busy_mode` picks the copy while a turn is running.
pub(super) fn composer_placeholder(
    disabled: bool,
    send_available: bool,
    running: bool,
    busy_mode: &str,
) -> &'static str {
    if disabled {
        "The agent host is offline"
    } else if !send_available {
        "Listening…"
    } else if running {
        match busy_mode {
            "follow_up" => "Add a follow-up…",
            "reject" => "This message will be rejected while busy",
            _ => "Steer the agent…",
        }
    } else {
        "Ask the agent, or type / for commands…"
    }
}

/// The goal, whether the run failed, and the session cost, read before the
/// submit closure takes the transcript.
pub(super) fn composer_run_facts(transcript: &Transcript) -> (Option<GoalState>, bool, f64) {
    (
        latest_goal(transcript),
        transcript.run_state == AgentRunState::Failed,
        transcript.usage.cost_usd,
    )
}

/// The transcript's most recent `/goal` state, if one was ever set.
pub(super) fn latest_goal(transcript: &Transcript) -> Option<GoalState> {
    transcript.items.iter().rev().find_map(|item| match item {
        TranscriptItem::Goal { state, .. } => Some(state.clone()),
        _ => None,
    })
}

pub(super) fn fill_draft(
    mut draft: Signal<String>,
    mut selected_option: Signal<usize>,
    text: &str,
) {
    draft.set(text.to_string());
    selected_option.set(0);
}

pub(super) fn focus_field(field: Signal<Option<Rc<MountedData>>>) {
    let Some(handle) = field.peek().clone() else {
        return;
    };
    spawn(async move {
        let _ = handle.set_focus(true).await;
    });
}

#[cfg(target_arch = "wasm32")]
pub(super) fn abort_run(pump: Pump, conversation_id: String) {
    wasm_bindgen_futures::spawn_local(async move {
        let request_id = super::request_ids::new_request_id();
        let abort = AbortAgentChat {
            conversation_id,
            request_id,
            background: Some(false),
        };
        if let Err(error) = pump.rpc().call(&abort).await {
            tracing::warn!(target: "agent_chat", %error, "agent abort failed");
        }
    });
}

/// Takes the edit-and-resend path when an edit is pending: refuses a slash
/// command (ambiguous rewrite-or-run), else rewinds to the edited message and
/// resubmits the edited text. Returns whether the caller's normal submit path
/// was consumed, so `false` means fall through to the ordinary composer send.
pub(super) fn try_submit_edit(
    edit_target: Signal<Option<EditTarget>>,
    draft: Signal<String>,
    pump: &Pump,
    conversation_id: &str,
    text: &str,
    staged_files: Signal<Vec<crate::components::terminal_chrome::attachment_picker::ChosenFile>>,
    report: EventHandler<Result<(), String>>,
) -> bool {
    let target_value = edit_target.peek();
    let Some(EditTarget::PendingRewind { item_id, .. }) = target_value.as_ref() else {
        return false;
    };
    if parse_slash_command(text).is_some() {
        report.call(Err("Finish or cancel the edit first.".to_owned()));
        return true;
    }
    let item_id = item_id.clone();
    drop(target_value);
    let attachments = staged_files.peek().clone();
    let pump = pump.clone();
    let conversation_id = conversation_id.to_owned();
    let text = text.to_owned();
    #[cfg(target_arch = "wasm32")]
    spawn(async move {
        let mut draft = draft;
        match super::composer_attachments::submit_attachments(&attachments).await {
            Ok(uploaded) => {
                let submitted_ids = attachments
                    .iter()
                    .filter_map(|file| file.upload_id.clone())
                    .collect::<Vec<_>>();
                let submitted_text = text.clone();
                let submitted_item_id = item_id.clone();
                let mut target_on_success = edit_target;
                let completion = EventHandler::new(move |result: Result<(), String>| {
                    if result.is_ok() {
                        super::composer_attachments::clear_submitted_files(
                            staged_files,
                            &submitted_ids,
                        );
                        if let Ok(mut target) = target_on_success.try_write()
                            && matches!(target.as_ref(), Some(EditTarget::PendingRewind { item_id, .. }) if item_id == &submitted_item_id)
                        {
                            *target = None;
                        }
                        if let Ok(mut draft) = draft.try_write()
                            && draft.trim() == submitted_text
                        {
                            draft.clear();
                        }
                    }
                    report.call(result);
                });
                super::message_actions::rewind_and_submit(
                    pump,
                    conversation_id,
                    item_id,
                    Some(text),
                    uploaded,
                    Some(completion),
                );
            }
            Err(error) => report.call(Err(error)),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (draft, staged_files);
        if attachments.is_empty() {
            super::message_actions::rewind_and_submit(
                pump,
                conversation_id,
                item_id,
                Some(text),
                Vec::new(),
                None,
            );
        } else {
            report.call(Err(
                "File attachments are available in the web client.".to_owned()
            ));
        }
    }
    true
}

/// The dismissible strip above the field while editing or holding restored attachments.
#[component]
pub(super) fn EditBanner(
    mut edit_target: Signal<Option<EditTarget>>,
    mut draft: Signal<String>,
) -> Element {
    let target = edit_target.read();
    let Some(target) = target.as_ref() else {
        return rsx! {};
    };
    let rewound = matches!(target, EditTarget::Rewound { .. });
    rsx! {
        div {
            class: "flex items-center gap-2 rounded-md border border-border bg-muted px-3 py-1.5 text-xs text-muted-foreground",
            "data-testid": "agent-chat-edit-banner",
            crate::components::ui::Badge { variant: crate::components::ui::BadgeVariant::Secondary,
                if rewound { "Restored" } else { "Editing" }
            }
            span { class: "flex-1",
                if rewound { "Restored message ready to send." } else { "Sending rewinds the conversation to this message." }
            }
            if target.has_attachments() {
                div { class: "flex flex-wrap gap-1",
                    match target {
                        EditTarget::PendingRewind { attachment_names, .. } => rsx! {
                            for filename in attachment_names {
                                crate::components::ui::Badge { variant: crate::components::ui::BadgeVariant::Outline, "{filename}" }
                            }
                        },
                        EditTarget::Rewound { attachments, .. } => rsx! {
                            for attachment in attachments {
                                crate::components::ui::Badge { variant: crate::components::ui::BadgeVariant::Outline, "{attachment.filename}" }
                            }
                        },
                    }
                }
            }
            crate::components::ui::IconButton {
                icon: crate::components::ui::IconType::X,
                label: "Cancel edit",
                title: "Cancel edit",
                size: crate::components::ui::ButtonSize::IconXs,
                variant: crate::components::ui::ButtonVariant::Ghost,
                onclick: move |_| {
                    edit_target.set(None);
                    draft.set(String::new());
                },
            }
        }
    }
}
