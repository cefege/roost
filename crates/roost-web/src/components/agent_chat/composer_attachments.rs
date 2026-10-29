//! Agent composer attachments reuse the terminal worker upload queue and shared progress chips.
//! Images stay inline in the submit payload; other file types submit their completed worker path.

use dioxus::prelude::*;
use dioxus::web::WebEventExt as _;
use roost_protocol::wire::agent_chat::ConversationSummary;

use crate::components::agent_chat::message_actions::EditTarget;
#[cfg(target_arch = "wasm32")]
use crate::components::terminal_chrome::attachment_picker::StagedUploadState;
use crate::components::terminal_chrome::attachment_picker::{
    AttachmentInput, ChosenFile, spawn_read_chosen,
};
use crate::components::terminal_chrome::composer_attachments::StagedAttachments;
use crate::components::terminal_chrome::dom;
use crate::components::terminal_chrome::terminal_upload::{
    retry_staged_upload_handler, start_staged_uploads,
};
use crate::components::ui::{ButtonSize, ButtonVariant, IconButton, IconType};
use crate::pump::Pump;
use roost_protocol::wire::agent_chat::{AgentChatAttachment, MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES};

/// Attach files to the worker immediately and expose their upload lifecycle.
#[component]
pub(super) fn AgentAttachmentControls(
    pump: Pump,
    conversation: ConversationSummary,
    staged_files: Signal<Vec<ChosenFile>>,
    on_chosen: EventHandler<Vec<ChosenFile>>,
    disabled: bool,
) -> Element {
    let mut file_input = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let retry = retry_staged_upload_handler(
        pump,
        conversation.id.clone(),
        conversation.worker_fp.clone(),
        staged_files,
    );
    let onmounted =
        EventHandler::new(move |event: MountedEvent| file_input.set(Some(event.data())));
    let staged = !staged_files().is_empty();
    rsx! {
        div { class: "flex min-w-0 flex-col items-start gap-2",
            AttachmentInput { on_chosen, onmounted: Some(onmounted) }
            IconButton {
                class: "shrink-0",
                icon: IconType::Paperclip,
                label: "Attach files",
                title: "Attach files",
                variant: ButtonVariant::Ghost,
                size: ButtonSize::IconSm,
                disabled,
                onclick: move |_| {
                    if let Some(input) = file_input.peek().as_ref() {
                        dom::open_file_chooser(input);
                    }
                },
            }
            if staged {
                StagedAttachments { staged_files, on_retry: retry }
            }
        }
    }
}

/// Files exposed by a browser's drop or clipboard transfer, preserving order.
pub(super) fn transferred_files(file_list: Option<web_sys::FileList>) -> Vec<web_sys::File> {
    let Some(file_list) = file_list else {
        return Vec::new();
    };
    (0..file_list.length())
        .filter_map(|index| file_list.item(index))
        .collect()
}

pub(super) fn attachment_drop_handler(
    on_attach: EventHandler<Vec<ChosenFile>>,
) -> EventHandler<DragEvent> {
    EventHandler::new(move |event: DragEvent| {
        event.prevent_default();
        let files = transferred_files(
            event
                .try_as_web_event()
                .and_then(|native: web_sys::DragEvent| native.data_transfer())
                .and_then(|transfer| transfer.files()),
        );
        spawn_read_chosen(files, on_attach);
    })
}

pub(super) fn attachment_paste_handler(
    on_attach: EventHandler<Vec<ChosenFile>>,
) -> EventHandler<ClipboardEvent> {
    EventHandler::new(move |event: ClipboardEvent| {
        let files = transferred_files(
            event
                .try_as_web_event()
                .and_then(|native: web_sys::ClipboardEvent| native.clipboard_data())
                .and_then(|transfer| transfer.files()),
        );
        if !files.is_empty() {
            event.prevent_default();
            spawn_read_chosen(files, on_attach);
        }
    })
}

/// Stages chosen files only when the current draft remains within the image wire budget.
pub(super) fn checked_stage_attachments(
    pump: Pump,
    staged_files: Signal<Vec<ChosenFile>>,
    conversation: ConversationSummary,
    edit_target: Signal<Option<EditTarget>>,
    draft: Signal<String>,
    report: EventHandler<Result<(), String>>,
) -> EventHandler<Vec<ChosenFile>> {
    EventHandler::new(move |chosen: Vec<ChosenFile>| {
        let estimated_bytes = {
            let target = edit_target.read();
            let retained = target
                .as_ref()
                .map_or(&[][..], EditTarget::retained_attachments);
            let staged = staged_files.read();
            let text = draft.read();
            estimate_staged_image_payload(&text, &staged, &chosen, retained)
        };
        if estimated_bytes.is_some_and(|size| size > MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES) {
            for file in &chosen {
                if let Some(preview) = &file.preview_url {
                    dom::revoke_preview(preview);
                }
            }
            report.call(Err(format!(
                "Selected images and this draft exceed the {} MiB message limit. Remove an image or shorten the draft.",
                MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES / (1024 * 1024)
            )));
            return;
        }
        report.call(Ok(()));
        start_staged_uploads(
            &pump,
            &conversation.id,
            &conversation.worker_fp,
            staged_files,
            chosen,
        );
    })
}

fn estimate_staged_image_payload(
    text: &str,
    staged: &[ChosenFile],
    chosen: &[ChosenFile],
    retained: &[AgentChatAttachment],
) -> Option<u64> {
    let mut total = text.len() as u64;
    let mut has_image = false;
    for file in staged.iter().chain(chosen) {
        let mime_type = file.file.type_();
        total = total
            .saturating_add(file.name.len() as u64)
            .saturating_add(mime_type.len() as u64)
            .saturating_add(
                file.uploaded_path
                    .as_ref()
                    .map_or(0, |path| path.len() as u64),
            )
            .saturating_add(32);
        if mime_type.starts_with("image/") {
            has_image = true;
            total = total.saturating_add(file.size_bytes);
        }
    }
    for attachment in retained {
        total = total
            .saturating_add(attachment.filename.len() as u64)
            .saturating_add(attachment.mime_type.len() as u64)
            .saturating_add(attachment.path.len() as u64)
            .saturating_add(32);
        if attachment.mime_type.starts_with("image/") {
            has_image = true;
            total = total.saturating_add(attachment.data.len() as u64);
        }
    }
    has_image.then_some(total)
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn submit_attachments(
    files: &[ChosenFile],
) -> Result<Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>, String> {
    let estimated_bytes = estimate_staged_image_payload("", &[], files, &[]);
    if estimated_bytes.is_some_and(|size| size > MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES) {
        return Err(format!(
            "Selected images exceed the {} MiB message limit.",
            MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES / (1024 * 1024)
        ));
    }
    use wasm_bindgen_futures::JsFuture;

    let mut attachments = Vec::with_capacity(files.len());
    for file in files {
        if !matches!(file.upload_state, StagedUploadState::Done) {
            return Err(format!("{} has not finished uploading", file.name));
        }
        let mime_type = file.file.type_();
        let path = file
            .uploaded_path
            .clone()
            .ok_or_else(|| format!("{} finished uploading without a worker path", file.name))?;
        let data = if mime_type.starts_with("image/") {
            let buffer = JsFuture::from(file.file.array_buffer())
                .await
                .map_err(|_| format!("Could not read {}", file.name))?;
            js_sys::Uint8Array::new(&buffer).to_vec()
        } else {
            Vec::new()
        };
        attachments.push(roost_protocol::wire::agent_chat::AgentChatAttachment {
            filename: file.name.clone(),
            mime_type,
            data,
            path,
        });
    }
    Ok(attachments)
}

/// Remove only attachments accepted by a successful submission.
pub(super) fn clear_submitted_files(
    mut staged_files: Signal<Vec<ChosenFile>>,
    submitted_upload_ids: &[String],
) {
    let mut previews = Vec::new();
    staged_files.with_mut(|files| {
        files.retain(|file| {
            let submitted = file
                .upload_id
                .as_ref()
                .is_some_and(|upload_id| submitted_upload_ids.contains(upload_id));
            if submitted {
                previews.extend(file.preview_url.iter().cloned());
            }
            !submitted
        });
    });
    for preview in previews {
        crate::components::terminal_chrome::dom::revoke_preview(&preview);
    }
}
