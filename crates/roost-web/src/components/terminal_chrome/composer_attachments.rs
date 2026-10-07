//! Staged file rows and their Send/Cancel actions for a terminal composer.
//! The owning pane supplies its shared staged-file signal and upload callback.
//! Removal revokes each object URL; confirmation transfers ownership to the upload queue.

use dioxus::prelude::*;

use super::attachment_picker::{AttachmentPreview, ChosenFile};
use super::dom;
use crate::components::md::{Button, ButtonSize, ButtonVariant};

/// Show staged files and actions above the composer's field.
#[component]
pub fn StagedAttachments(
    staged_files: Signal<Vec<ChosenFile>>,
    on_send_uploads: EventHandler<Vec<ChosenFile>>,
) -> Element {
    let staged = staged_files();
    if staged.is_empty() {
        return rsx! {};
    }
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
        AttachmentPreview {
            files: staged.clone(),
            on_remove: remove,
        }
        div {
            style: "display: flex; justify-content: flex-end; gap: var(--md-space-2);",
            Button {
                variant: ButtonVariant::Outline,
                size: ButtonSize::Sm,
                onclick: move |_| clear_staged_files(staged_files),
                "Cancel"
            }
            Button {
                variant: ButtonVariant::Default,
                size: ButtonSize::Sm,
                icon: "send",
                onclick: move |_| send_staged_files(staged_files, on_send_uploads),
                "Send {staged.len()} files"
            }
        }
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

/// Hand the staged files to the upload queue after Send.
pub fn send_staged_files(
    mut staged_files: Signal<Vec<ChosenFile>>,
    on_send_uploads: EventHandler<Vec<ChosenFile>>,
) {
    let files = std::mem::take(&mut *staged_files.write());
    if !files.is_empty() {
        on_send_uploads.call(files);
    }
}
