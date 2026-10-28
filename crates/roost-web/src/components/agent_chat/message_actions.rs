//! Retry and rewind controls attached to transcript messages.
//! Rewind changes the transcript before any draft is restored to the composer.

use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use dioxus::prelude::*;

use crate::components::ui::{
    Button, ButtonVariant, Checkbox, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader,
    DialogTitle, IconType, Label,
};
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use super::request_ids::new_request_id;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{RewindAgentChat, SubmitAgentChat};

#[derive(Debug, Clone, PartialEq)]
pub enum EditTarget {
    PendingRewind {
        item_id: String,
        attachment_names: Vec<String>,
    },
    // Only the browser RPC completion constructs a restored draft.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Rewound {
        item_id: String,
        attachments: Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>,
    },
}

impl EditTarget {
    pub fn has_attachments(&self) -> bool {
        match self {
            Self::PendingRewind {
                attachment_names, ..
            } => !attachment_names.is_empty(),
            Self::Rewound { attachments, .. } => !attachments.is_empty(),
        }
    }

    pub fn retained_attachments(&self) -> &[roost_protocol::wire::agent_chat::AgentChatAttachment] {
        match self {
            Self::PendingRewind { .. } => &[],
            Self::Rewound { attachments, .. } => attachments,
        }
    }
}

struct RewindDraftSource {
    item_id: String,
    text: String,
    attachment_names: Vec<String>,
}

#[component]
pub fn UserMessageActions(
    conversation_id: String,
    item_id: String,
    text: String,
    attachment_names: Vec<String>,
    draft: Signal<String>,
    edit_target: Signal<Option<EditTarget>>,
    running: bool,
    pump: Pump,
) -> Element {
    let edit_names = attachment_names.clone();
    let mut confirm = use_signal(|| false);
    let mut restore_files = use_signal(|| true);
    let retry_pump = pump.clone();
    let retry_id = conversation_id.clone();
    let retry_item = item_id.clone();
    let retry_text = None;
    let rewind_pump = pump.clone();
    let rewind_id = conversation_id.clone();
    let rewind_item = item_id.clone();
    let mut edit_draft = draft;
    let edit_item = item_id.clone();
    let edit_text = text.clone();
    let rewind_text = text.clone();
    let rewind_names = attachment_names.clone();
    let mut items = Vec::with_capacity(3);
    if !running {
        items.push(ToolbarMenuItem::choice("edit", "Edit", false));
    }
    items.push(ToolbarMenuItem::choice("retry", "Retry", false));
    items.push(ToolbarMenuItem::choice("rewind", "Rewind to here", false));
    rsx! {
        ToolbarMenu {
            menu_key: format!("message-{item_id}"),
            trigger: ToolbarTrigger::Icon { icon: IconType::Ellipsis },
            aria_label: "Message actions",
            items,
            on_choose: move |action: String| match action.as_str() {
                "edit" if !running => {
                    edit_draft.set(edit_text.clone());
                    edit_target.set(Some(EditTarget::PendingRewind {
                        item_id: edit_item.clone(),
                        attachment_names: edit_names.clone(),
                    }));
                }
                "retry" => rewind_and_submit(retry_pump.clone(), retry_id.clone(), retry_item.clone(), retry_text.clone(), Vec::new(), None),
                "rewind" => confirm.set(true),
                _ => {}
            },
        }
        Dialog { open: confirm(), on_open_change: move |open: bool| confirm.set(open),
            DialogContent {
                DialogHeader {
                    DialogTitle { "Rewind conversation?" }
                }
                DialogBody {
                    p { class: "text-sm text-muted-foreground", "Messages after this point will be removed from the conversation." }
                    Label { class: "flex items-center gap-2 text-sm font-normal leading-normal",
                        Checkbox {
                            checked: restore_files(),
                            aria_label: "Restore files changed after this message",
                            on_checked_change: move |value: bool| restore_files.set(value),
                        }
                        "Restore files changed after this message"
                    }
                }
                DialogFooter {
                    Button { variant: ButtonVariant::Outline, onclick: move |_| confirm.set(false), "Cancel" }
                    Button {
                        variant: ButtonVariant::Destructive,
                        onclick: move |_| {
                            confirm.set(false);
                            rewind_to(
                                rewind_pump.clone(),
                                rewind_id.clone(),
                                restore_files(),
                                RewindDraftSource {
                                    item_id: rewind_item.clone(),
                                    text: rewind_text.clone(),
                                    attachment_names: rewind_names.clone(),
                                },
                                draft,
                                edit_target,
                            );
                        },
                        "Rewind"
                    }
                }
            }
        }
    }
}

/// Rewinds to `item_id` and resubmits the original attachments with the selected text.
pub(super) fn rewind_and_submit(
    pump: Pump,
    conversation_id: String,
    item_id: String,
    replacement_text: Option<String>,
    extra_attachments: Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>,
    on_result: Option<EventHandler<Result<(), String>>>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let (sent, answer) = futures::channel::oneshot::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let result = match pump
                .rpc()
                .call(&RewindAgentChat {
                    conversation_id: conversation_id.clone(),
                    item_id,
                    restore_files: false,
                    request_id: new_request_id(),
                })
                .await
            {
                Ok(original) => {
                    let mut attachments = original.attachments;
                    attachments.extend(extra_attachments);
                    let text = replacement_text.unwrap_or(original.text);
                    match roost_protocol::wire::agent_chat::validate_agent_chat_image_payload(
                        &text,
                        &attachments,
                    ) {
                        Ok(()) => submit_message(pump, conversation_id, text, attachments).await,
                        Err(error) => Err(error.to_string()),
                    }
                }
                Err(error) => Err(error.to_string()),
            };
            let _ = sent.send(result);
        });
        spawn(async move {
            if let Ok(result) = answer.await {
                if let Some(on_result) = on_result {
                    on_result.call(result);
                } else if let Err(error) = result {
                    tracing::warn!(target: "agent_chat", %error, "agent rewind-and-submit failed");
                }
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (
            pump,
            conversation_id,
            item_id,
            replacement_text,
            extra_attachments,
        );
        if let Some(on_result) = on_result {
            on_result.call(Err(
                "Message editing is available in the web client.".to_owned()
            ));
        }
    }
}

fn rewind_to(
    pump: Pump,
    conversation_id: String,
    restore_files: bool,
    source: RewindDraftSource,
    draft: Signal<String>,
    edit_target: Signal<Option<EditTarget>>,
) {
    let RewindDraftSource {
        item_id,
        text: fallback_text,
        attachment_names,
    } = source;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut draft = draft;
        let mut edit_target = edit_target;
        match pump
            .rpc()
            .call(&RewindAgentChat {
                conversation_id,
                item_id: item_id.clone(),
                restore_files,
                request_id: new_request_id(),
            })
            .await
        {
            Ok(message) => {
                let pending = message.text.is_empty()
                    && message.attachments.is_empty()
                    && (!fallback_text.is_empty() || !attachment_names.is_empty());
                let restored_text = if pending { fallback_text } else { message.text };
                if let Ok(mut draft) = draft.try_write() {
                    *draft = restored_text;
                }
                if let Ok(mut target) = edit_target.try_write() {
                    *target = Some(if pending {
                        EditTarget::PendingRewind {
                            item_id,
                            attachment_names,
                        }
                    } else {
                        EditTarget::Rewound {
                            item_id,
                            attachments: message.attachments,
                        }
                    });
                }
            }
            Err(error) => tracing::warn!(target: "agent_chat", %error, "agent rewind failed"),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (
        pump,
        conversation_id,
        item_id,
        restore_files,
        fallback_text,
        attachment_names,
        draft,
        edit_target,
    );
}

#[cfg(target_arch = "wasm32")]
async fn submit_message(
    pump: Pump,
    conversation_id: String,
    text: String,
    attachments: Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>,
) -> Result<(), String> {
    pump.rpc()
        .call(&SubmitAgentChat {
            conversation_id,
            text,
            request_id: new_request_id(),
            when_busy: String::new(),
            attachments,
        })
        .await
        .map_err(|error| error.to_string())
}
