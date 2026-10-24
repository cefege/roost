//! Staged file rows and their Send/Cancel actions for a terminal composer.
//! The owning pane supplies its shared staged-file signal and upload callback.
//! Removal revokes each object URL; confirmation transfers ownership to the upload queue.

use dioxus::prelude::*;

use super::attachment_picker::{AttachmentPreview, ChosenFile};
use super::dom;
use libero::components::{Box, Button};
use libero::sx::sx;

/// Show staged files and actions above the composer's field.
#[component]
pub fn StagedAttachments(
    staged_files: Signal<Vec<ChosenFile>>,
    on_send_uploads: EventHandler<Vec<ChosenFile>>,
) -> Element {
    let container_sx = use_hook(|| sx().display("contents").pointer_events("auto"));
    let actions_sx = use_hook(|| sx().display("flex").justify_content("end").gap("sm"));
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
        Box { component: "div", sx: container_sx,
            AttachmentPreview {
                files: staged.clone(),
                on_remove: remove,
            }
            Box { component: "div", sx: actions_sx,
                Button {
                    variant: "outlined",
                    size: "sm",
                    onclick: move |_| clear_staged_files(staged_files),
                    "Cancel"
                }
                Button {
                    variant: "filled",
                    size: "sm",
                    icon: rsx! { libero::components::Pictogram { icon: pictogram_icons_lucide::send::outlined, width: "1rem", height: "1rem" } },
                    onclick: move |_| send_staged_files(staged_files, on_send_uploads),
                    if staged.len() == 1 { "Send 1 file" } else { "Send {staged.len()} files" }
                }
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
