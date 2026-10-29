//! Staged thumbnails and upload status inside the terminal composer.
//! The owning pane supplies the staged-file signal; Send submits completed
//! paths and Escape/removal abandons files without typing their paths.

use std::collections::BTreeSet;

use dioxus::prelude::*;

use super::attachment_picker::{ChosenFile, StagedUploadState};
use super::dom;
use super::upload_tray::format_file_size;
use crate::components::notifications::inline_transfer::use_inline_transfer_presentation;
use crate::components::ui::AttachmentState;
use crate::components::ui::attachment_upload::{
    AttachmentUploadChipData, AttachmentUploadChips, AttachmentUploadPresentation,
};

/// Show staged files with their live upload state.
#[component]
pub fn StagedAttachments(
    staged_files: Signal<Vec<ChosenFile>>,
    on_retry: EventHandler<usize>,
    #[props(default)] compact_image_uploads: bool,
    #[props(into, optional)] inline_transfer_scope: Option<String>,
    #[props(default = true)] visible: bool,
) -> Element {
    let staged = staged_files();
    let inline_presentation = use_inline_transfer_presentation();
    let claim_scope = inline_transfer_scope.clone();
    use_effect(use_reactive(
        (&visible, &compact_image_uploads, &claim_scope),
        move |(visible, compact_image_uploads, claim_scope)| {
            let image_upload_ids = if visible && compact_image_uploads {
                staged_files
                    .read()
                    .iter()
                    .filter(|file| file.preview_url.is_some())
                    .filter_map(|file| file.upload_id.clone())
                    .collect::<BTreeSet<_>>()
            } else {
                BTreeSet::new()
            };
            if let (Some(scope), Some(presentation)) = (claim_scope.clone(), inline_presentation) {
                presentation.replace(scope, image_upload_ids);
            }
        },
    ));
    use_drop(move || {
        if let (Some(scope), Some(presentation)) = (inline_transfer_scope, inline_presentation) {
            presentation.clear(&scope);
        }
    });
    if staged.is_empty() {
        return rsx! {};
    }
    let chips = staged.iter().map(upload_chip_data).collect::<Vec<_>>();
    let remove = move |index: usize| {
        staged_files.with_mut(|files| {
            if index < files.len() {
                let removed = files.remove(index);
                if let Some(url) = removed.preview_url.as_deref() {
                    dom::revoke_preview(url);
                }
            }
        });
    };
    rsx! {
        AttachmentUploadChips {
            files: chips,
            on_remove: remove,
            on_retry: move |index| on_retry.call(index),
            presentation: if compact_image_uploads {
                AttachmentUploadPresentation::CompactImages
            } else {
                AttachmentUploadPresentation::Standard
            },
        }
    }
}

fn upload_chip_data(file: &ChosenFile) -> AttachmentUploadChipData {
    let (status, progress, in_flight, done, retryable, state) = match &file.upload_state {
        StagedUploadState::Staged | StagedUploadState::Queued => (
            "Queued…".to_owned(),
            None,
            true,
            false,
            false,
            AttachmentState::Uploading,
        ),
        StagedUploadState::Hashing => (
            "Checking…".to_owned(),
            None,
            true,
            false,
            false,
            AttachmentState::Uploading,
        ),
        StagedUploadState::Uploading { bytes_sent } => (
            format!(
                "{} of {}",
                format_file_size(*bytes_sent),
                format_file_size(file.size_bytes)
            ),
            (file.size_bytes > 0).then_some(*bytes_sent as f64 / file.size_bytes as f64),
            true,
            false,
            false,
            AttachmentState::Uploading,
        ),
        StagedUploadState::Done => (
            "Ready to send".to_owned(),
            Some(1.0),
            false,
            true,
            false,
            AttachmentState::Done,
        ),
        StagedUploadState::Failed { reason, retryable } => (
            format!("Failed · {reason}"),
            None,
            false,
            false,
            *retryable,
            AttachmentState::Error,
        ),
    };
    AttachmentUploadChipData {
        name: file.name.clone(),
        size_label: format_file_size(file.size_bytes),
        preview_url: file.preview_url.clone(),
        status,
        progress,
        in_flight,
        done,
        retryable,
        state,
    }
}
/// Whether staged files are uploading or have failed.
pub fn upload_send_state(files: &[ChosenFile]) -> (bool, bool) {
    let pending = files.iter().any(|file| file.upload_state.is_in_flight());
    let failed = files
        .iter()
        .any(|file| matches!(&file.upload_state, StagedUploadState::Failed { .. }));
    (pending, failed)
}
/// Whether any staged upload must be resolved before Send can proceed.
pub fn staged_files_block_send(files: &[ChosenFile]) -> bool {
    files.iter().any(|file| file.upload_state.blocks_send())
}

/// The Send button's disabled state and the reason it cannot send.
pub fn send_button_state(base_disabled: bool, pending: bool, failed: bool) -> (bool, &'static str) {
    let title = if pending {
        "Uploading…"
    } else if failed {
        "Remove failed uploads before sending"
    } else {
        "Send to terminal"
    };
    (base_disabled || pending || failed, title)
}
/// Prefix completed worker paths to the draft in the composer's established order.
pub fn compose_terminal_submission(files: &[ChosenFile], draft: &str) -> String {
    let paths = files
        .iter()
        .filter_map(|file| file.uploaded_path.as_deref())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        draft.to_owned()
    } else if draft.is_empty() {
        paths.join(" ")
    } else {
        format!("{} {draft}", paths.join(" "))
    }
}

/// Clear the staged set and release its image previews.
pub fn clear_staged_files(mut staged_files: Signal<Vec<ChosenFile>>) {
    let files = std::mem::take(&mut *staged_files.write());
    for file in files {
        if let Some(url) = file.preview_url.as_deref() {
            dom::revoke_preview(url);
        }
    }
}
