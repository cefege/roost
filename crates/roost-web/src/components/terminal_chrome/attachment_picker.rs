//! Read chosen files and create local image previews for staged attachments.
//! The hidden input stays beside the composer's attach button because iOS
//! rejects programmatic clicks outside the gesture that requested them.

use dioxus::prelude::*;

use super::dom;
use super::upload_tray::{extension_label, format_file_size};
use crate::components::ui::field::scoped_element_id;
use crate::components::ui::{
    Attachment, AttachmentActions, AttachmentContent, AttachmentDescription, AttachmentGroup,
    AttachmentMedia, AttachmentMediaVariant, AttachmentOrientation, AttachmentSize,
    AttachmentTitle, ButtonSize, ButtonVariant, Icon, IconButton, IconType,
};

/// One chosen file: its name, length and the browser's handle to it. The
/// bytes are read a chunk at a time while the upload runs, so a large video is
/// never held whole in the tab's memory.
#[derive(Debug, Clone, PartialEq)]
pub struct ChosenFile {
    /// The name the worker will store it under.
    pub name: String,
    /// Its byte length.
    pub size_bytes: u64,
    /// The browser's handle, read through `dom::read_file_range`.
    pub file: web_sys::File,
    /// A local image URL owned by staging. Removal or upload handoff releases it
    /// through [`crate::components::terminal_chrome::dom::revoke_preview`].
    pub preview_url: Option<String>,
}

/// The `multiple` file input the composer's attach button clicks.
///
/// Hidden with `display: none` rather than `visibility: hidden` so it is out
/// of the focus order and out of the accessibility tree entirely; the button
/// that owns the gesture is the control a reader announces.
#[component]
pub fn AttachmentInput(
    #[props(default)] accept: Option<String>,
    on_chosen: EventHandler<Vec<ChosenFile>>,
    onmounted: Option<EventHandler<MountedEvent>>,
) -> Element {
    let read = move |event: FormEvent| {
        spawn_read_chosen(dom::files_of_event(&event), on_chosen);
    };
    let mounted = move |event: MountedEvent| {
        if let Some(handler) = onmounted {
            handler.call(event);
        }
    };
    rsx! {
        input {
            r#type: "file",
            multiple: true,
            accept: accept,
            style: "display: none;",
            "data-testid": "chat-file-input",
            onmounted: mounted,
            onchange: read,
        }
    }
}

/// Turn a picker result or a drop into `ChosenFile`s, in the order the user
/// picked them.
///
/// The desktop contract attaches several files in one gesture and the worker
/// stores them in that order, so the order is the payload and not an accident.
pub(super) fn spawn_read_chosen(
    files: Vec<web_sys::File>,
    on_chosen: EventHandler<Vec<ChosenFile>>,
) {
    // Off the render path, because minting a preview is asynchronous.
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let chosen = read_chosen(files).await;
        if !chosen.is_empty() {
            on_chosen.call(chosen);
        }
    });
    // A host with no file picker has nothing to read.
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (files, on_chosen);
}

/// Read `files` into `ChosenFile`s, previews minted, in the order given. The
/// one reader behind the picker, a drop, and a paste.
#[cfg(target_arch = "wasm32")]
pub(super) async fn read_chosen(files: Vec<web_sys::File>) -> Vec<ChosenFile> {
    let mut chosen = Vec::with_capacity(files.len());
    for file in files {
        chosen.push(ChosenFile {
            name: dom::file_name(&file),
            size_bytes: dom::file_size(&file),
            preview_url: dom::preview_url(&file).await,
            file,
        });
    }
    chosen
}

/// A single scrolling row of attachments inside the prompt, above its field.
#[component]
pub fn AttachmentPreview(files: Vec<ChosenFile>, on_remove: EventHandler<usize>) -> Element {
    let label_id = use_hook(|| scoped_element_id("attachment-preview-strip"));
    if files.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "w-full min-w-0 px-3 pt-3",
            "data-testid": "attachment-preview-strip",
            "data-composer-part": "attachments",
            role: "list",
            "aria-labelledby": label_id.clone(),
            span { id: label_id, class: "sr-only", "Attachments staged for sending" }
            AttachmentGroup {
                for (index, file) in files.into_iter().enumerate() {
                    StagedAttachmentRow { key: "{index}-{file.name}", file, index, on_remove }
                }
            }
        }
    }
}

#[component]
fn StagedAttachmentRow(file: ChosenFile, index: usize, on_remove: EventHandler<usize>) -> Element {
    let name = file.name.clone();
    let size_label = format_file_size(file.size_bytes);
    let is_video = video_file(&file);
    let has_preview = file.preview_url.is_some();
    rsx! {
        Attachment {
            size: if has_preview { AttachmentSize::Default } else { AttachmentSize::Sm },
            orientation: if has_preview { AttachmentOrientation::Vertical } else { AttachmentOrientation::Horizontal },
            class: if has_preview { "" } else { "w-48" },
            "tabindex": "0",
            "role": "listitem",
            "data-testid": "staged-attachment",
            "aria-label": "{name}, {size_label}",
            title: "{name}, {size_label}",
            if let Some(source) = file.preview_url.as_ref() {
                AttachmentMedia { variant: AttachmentMediaVariant::Image,
                    contain: true,
                    img {
                        "data-testid": "attachment-preview",
                        src: source,
                        alt: name.clone(),
                        class: "size-full",
                    }
                }
            } else {
                AttachmentMedia { variant: AttachmentMediaVariant::Icon, class: "text-muted-foreground",
                    if is_video {
                        Icon { icon: IconType::Film }
                    } else {
                        "{extension_label(&file.name)}"
                    }
                }
            }
            if !has_preview {
                AttachmentContent {
                    AttachmentTitle { "title": name.clone(), "{name}" }
                    AttachmentDescription { "{size_label}" }
                }
            }
            AttachmentActions {
                IconButton {
                    icon: IconType::X,
                    label: "Remove {name}",
                    title: "Remove {name}",
                    variant: if has_preview { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                    size: ButtonSize::IconXs,
                    onclick: move |_| on_remove.call(index),
                }
            }
        }
    }
}

fn video_file(file: &ChosenFile) -> bool {
    file.file.type_().starts_with("video/")
        || file.name.rsplit_once('.').is_some_and(|(_, extension)| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp4" | "mov" | "webm" | "mkv" | "avi" | "m4v"
            )
        })
}
