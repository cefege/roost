//! Read chosen files and create local image previews for staged attachments.
//! The hidden input stays beside the composer's attach button because iOS
//! rejects programmatic clicks outside the gesture that requested them.

use dioxus::prelude::*;
use libero::components::Pictogram;

use super::dom;
use super::upload_tray::{extension_label, format_file_size};

use crate::components::ui::{
    Attachment, AttachmentActions, AttachmentContent, AttachmentDescription, AttachmentMedia,
    AttachmentMediaVariant, AttachmentSize, AttachmentTitle,
};
use libero::components::{ActionIcon, Box, Paper, Text};
use libero::sx::sx;

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
    /// A local object URL for this file's image preview, when available. The
    /// staged tray or transfer row releases it with
    /// [`crate::components::terminal_chrome::dom::revoke_preview`].
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

/// The staged attachments and their keyboard-accessible removal controls.
#[component]
pub fn AttachmentPreview(files: Vec<ChosenFile>, on_remove: EventHandler<usize>) -> Element {
    let label_id = libero::hooks::use_id().read().clone();
    let surface_sx = use_hook(|| sx().padding("xs"));
    let screen_reader_sx = use_hook(|| {
        sx().position("absolute")
            .width("1px")
            .height("1px")
            .padding("0")
            .margin("-1px")
            .overflow("hidden")
            .with("clip", "rect(0, 0, 0, 0)")
            .white_space("nowrap")
            .with("border", "0")
    });
    let rows_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("xs"));
    if files.is_empty() {
        return rsx! {};
    }
    rsx! {
        Paper {
            shadow: "sm",
            radius: "sm",
            bordered: true,
            sx: surface_sx,
            "data-testid": "attachment-preview-strip",
            role: "list",
            aria_labelledby: label_id.clone(),
            Text { component: "span", id: label_id, sx: screen_reader_sx, "Attachments staged for sending" }
            Box { component: "div", sx: rows_sx,
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
    let attachment_sx = use_hook(|| sx().width("100%").min_width("0").border_radius("md"));
    let media_sx = use_hook(|| sx().font_size("xs").color("muted"));
    rsx! {
        Attachment {
            size: AttachmentSize::Sm,
            sx: attachment_sx,
            "tabindex": "0",
            "role": "listitem",
            "data-testid": "staged-attachment",
            "aria-label": "{name}, {size_label}",
            if let Some(source) = file.preview_url.as_ref() {
                AttachmentMedia { variant: AttachmentMediaVariant::Image,
                    img {
                        "data-testid": "attachment-preview",
                        src: source,
                        alt: "",
                    }
                }
            } else {
                AttachmentMedia { variant: AttachmentMediaVariant::Icon, sx: media_sx,
                    if is_video {
                        Pictogram { icon: pictogram_icons_lucide::film::outlined }
                    } else {
                        "{extension_label(&file.name)}"
                    }
                }
            }
            AttachmentContent {
                AttachmentTitle { "title": name.clone(), "{name}" }
                AttachmentDescription { "{size_label}" }
            }
            AttachmentActions {
                ActionIcon {
                    icon: pictogram_icons_lucide::x::outlined,
                    aria_label: "Remove {name}",
                    title: "Remove {name}",
                    variant: "standard",
                    size: "sm",
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
