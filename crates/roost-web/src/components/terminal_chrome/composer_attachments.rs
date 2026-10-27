//! Staged thumbnails inside the terminal composer.
//! The owning pane supplies its shared staged-file signal; the composer's
//! existing Send and Escape controls own confirmation and cancellation.

use dioxus::prelude::*;

use super::attachment_picker::{AttachmentPreview, ChosenFile};
use super::dom;

/// Show staged files inside the prompt, without a second Send/Cancel toolbar.
#[component]
pub fn StagedAttachments(staged_files: Signal<Vec<ChosenFile>>) -> Element {
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
        AttachmentPreview { files: staged, on_remove: remove }
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
