//! Reading what the user picked out of the file input, and the previews of
//! those choices. The input itself is a hidden sibling of the composer's attach
//! button — v2 wired the same way, with the parent's own ref — because iOS
//! refuses a programmatic click that is not inside the gesture that asked for
//! it.
//! Ports the picker side of
//! `apps/web/src/components/terminal/TerminalComposeButton.tsx`'s
//! `onAttachFiles`, and the local preview of `apps/web/src/client/attachments.ts`.

use dioxus::prelude::*;

use super::dom;
use crate::components::md::focus_scope::VISUALLY_HIDDEN_STYLE;
use crate::components::md::form_field::scoped_element_id;
use crate::components::md::{Surface, SurfaceRadius};

/// One chosen file, read out of the browser before it is handed upward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenFile {
    /// The name the worker will store it under.
    pub name: String,
    /// Its byte length.
    pub size_bytes: u64,
    /// Its bytes, read once so the chunker never re-reads a moving source.
    pub bytes: Vec<u8>,
    /// A local object URL for the transfer card's preview, when the browser
    /// could mint one. The card's owner releases it with
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

/// Read a picker result into `ChosenFile`s, in the order the user picked them.
///
/// The desktop contract attaches several files in one gesture and the worker
/// stores them in that order, so the order is the payload and not an accident.
fn spawn_read_chosen(files: Vec<web_sys::File>, on_chosen: EventHandler<Vec<ChosenFile>>) {
    // The read is off the render path: a picked file is read in full before it
    // can be chunked, and a component that awaited it would paint nothing until
    // the browser finished.
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut chosen = Vec::with_capacity(files.len());
        for file in files {
            let Some(bytes) = dom::read_bytes(&file).await else {
                continue;
            };
            chosen.push(ChosenFile {
                name: dom::file_name(&file),
                size_bytes: dom::file_size(&file),
                bytes,
                preview_url: dom::preview_url(&file).await,
            });
        }
        if !chosen.is_empty() {
            on_chosen.call(chosen);
        }
    });
    // A host with no file picker has nothing to read.
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (files, on_chosen);
}

/// The previews of the files an upload has in flight, above the composer field.
///
/// The preview is a LOCAL object URL minted at pick time, so an image is on
/// screen while the coordinator's dedup probe is still held — waiting for the
/// worker to answer would show nothing at all for the slow half of an upload.
#[component]
pub fn AttachmentPreview(files: Vec<ChosenFile>) -> Element {
    let label_id = use_hook(|| scoped_element_id("attachment-preview-strip"));
    if files.is_empty() {
        return rsx! {};
    }
    rsx! {
        Surface {
            level: 2,
            elevation: 2,
            radius: SurfaceRadius::Sm,
            test_id: Some("attachment-preview-strip".to_owned()),
            role: Some("group".to_owned()),
            aria_labelledby: Some(label_id.clone()),
            style: "display: flex; flex-wrap: wrap; gap: var(--md-space-2); padding: var(--md-space-2);".to_owned(),
            span { id: label_id, style: VISUALLY_HIDDEN_STYLE, "Attachments" }
            for file in files {
                span {
                    class: "md-label-s",
                    title: file.name.clone(),
                    if let Some(source) = file.preview_url.as_ref() {
                        img {
                            "data-testid": "attachment-preview",
                            src: source,
                            alt: "",
                            style: "width: var(--md-space-9); height: var(--md-space-9); border-radius: var(--md-shape-sm); object-fit: cover;",
                        }
                    }
                    "{file.name}"
                }
            }
        }
    }
}
