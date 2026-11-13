//! Browser files enter OMP through its real worker upload and pasted-path route.
//! Only the canonical main composer opts in; settings fields remain text-only.
//! Upload receipts fence queued actions until the owning input can receive them.
use std::{collections::BTreeMap, rc::Rc};

use dioxus::prelude::*;
use serde_json::json;

use super::super::{media::MediaContext, text_sync::TextSync};
use crate::components::terminal_chrome::{
    attachment_picker::{AttachmentInput, ChosenFile},
    dom as upload_dom,
    upload::{UploadContext, UploadUpdate, enqueue_attachments},
    upload_host::DirectIdentity,
    upload_id::mint_upload_id,
};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, IconButton, IconType};
use crate::pump::use_store;

#[path = "editor_attachment_dom.rs"]
mod dom;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct NativeAttachments {
    pub main: Signal<bool>,
    pub transfers: Signal<BTreeMap<String, Option<String>>>,
    pub failure: Signal<Option<String>>,
    pub chosen: EventHandler<Vec<ChosenFile>>,
    pub reading: EventHandler<()>,
    pub discard_failed: EventHandler<()>,
}

pub(super) fn use_attachments(
    element_id: &str,
    surface: &str,
    node_id: &str,
    enabled: bool,
) -> NativeAttachments {
    let pump = use_store();
    let media = use_context::<Signal<MediaContext>>();
    let sync = use_context::<TextSync>();
    let mut transfers = use_signal(BTreeMap::<String, Option<String>>::new);
    let mut local_failure = use_signal(|| None::<String>);
    let main = use_signal(|| false);
    let mut reading_count = use_signal(|| 0usize);
    let reading_key = format!("omp-attachment-read:{element_id}");
    let begin_sync = sync.clone();
    let begin_key = reading_key.clone();
    let reading = use_callback(move |()| {
        reading_count.with_mut(|count| *count += 1);
        begin_sync.pending(&begin_key, true);
    });
    let field_id = element_id.to_owned();
    let focus_surface = surface.to_owned();
    let focus_node = node_id.to_owned();
    let finish_sync = sync.clone();
    let finished = use_callback(move |(upload_id, path): (String, String)| {
        if !transfers.peek().contains_key(&upload_id) {
            return;
        }
        let data = match attachment_input(&path) {
            Ok(data) if dom::focus_composer(&field_id) => data,
            Ok(_) => {
                transfers.write().insert(upload_id, Some("The uploaded file could not reach its OMP composer. Discard it and pick it again when the composer is available.".into()));
                return;
            }
            Err(message) => {
                transfers.write().insert(upload_id, Some(message.into()));
                return;
            }
        };
        let focus = super::super::event("focus", &focus_surface, &focus_node, json!({}));
        if finish_sync.prerequisite_pair(focus, json!({"method":"input","params":{"data":data}})) {
            transfers.write().remove(&upload_id);
            finish_sync.pending(&fence(&upload_id), false);
        } else {
            transfers.write().insert(upload_id, Some("The file was uploaded, but OMP could not queue its attachment. Discard it and pick it again.".into()));
        }
    });
    let failed = use_callback(move |(upload_id, message): (String, String)| {
        if transfers.peek().contains_key(&upload_id) {
            transfers.write().insert(upload_id, Some(message));
        }
    });
    let queue_sync = sync.clone();
    let queue_reading_key = reading_key.clone();
    let chosen = use_callback(move |mut files: Vec<ChosenFile>| {
        reading_count.with_mut(|count| *count = count.saturating_sub(1));
        for file in &mut files {
            if let Some(url) = file.preview_url.take() {
                upload_dom::revoke_preview(&url);
            }
            file.upload_id = mint_upload_id();
        }
        if !*main.peek() {
            local_failure.set(Some("The OMP composer is unavailable. No files were uploaded; discard the selection and try again.".into()));
            return;
        }
        if files.iter().any(|file| file.upload_id.is_none()) {
            local_failure.set(Some(
                "This page cannot create a secure upload ID. No files were uploaded.".into(),
            ));
            return;
        }
        let owner = media.peek().clone();
        let context = UploadContext {
            session_id: owner.conversation_id,
            worker_fp: Some(owner.worker_fp),
            short_path: false,
            raw_worker_path: true,
            identity: DirectIdentity {
                tab_id: pump.core().borrow().store().tab_id.clone(),
                device_fingerprint: pump
                    .rpc()
                    .device_key()
                    .map(|key| key.fingerprint().to_owned())
                    .unwrap_or_default(),
            },
        };
        for file in &files {
            if let Some(upload_id) = &file.upload_id {
                transfers.write().insert(upload_id.clone(), None);
                queue_sync.pending(&fence(upload_id), true);
            }
        }
        tracing::info!(target: "attachments", conversation_id = %context.session_id, files = files.len(), "OMP composer attachments queued");
        enqueue_attachments(
            &pump,
            &context,
            files,
            Rc::new(move |upload_id, path| finished.call((upload_id.to_owned(), path.to_owned()))),
            Some(Rc::new(move |upload_id, update| {
                if let UploadUpdate::Failed { reason, .. } = update {
                    failed.call((upload_id.to_owned(), reason));
                }
            })),
        );
        if local_failure.peek().is_none() {
            queue_sync.pending(&queue_reading_key, *reading_count.peek() != 0);
        }
    });
    dom::use_files(element_id, enabled, chosen, reading, main);
    let discard_sync = sync.clone();
    let discard_reading_key = reading_key.clone();
    let discard_failed = use_callback(move |()| {
        local_failure.set(None);
        let failures = transfers
            .peek()
            .iter()
            .filter(|(_, error)| error.is_some())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        discard_sync.abandon_queued_actions(
            "Attachment upload discarded. Your draft was kept; send it again when ready.",
        );
        for id in failures {
            transfers.write().remove(&id);
            discard_sync.pending(&fence(&id), false);
        }
        discard_sync.pending(&discard_reading_key, *reading_count.peek() != 0);
    });
    use_drop(move || {
        for upload_id in transfers.peek().keys() {
            sync.cancel(&fence(upload_id));
        }
        sync.cancel(&reading_key);
    });
    NativeAttachments {
        main,
        transfers,
        failure: local_failure,
        chosen,
        reading,
        discard_failed,
    }
}

#[component]
pub(super) fn AttachmentControls(owner: NativeAttachments, disabled: bool) -> Element {
    let mut input = use_signal(|| None::<Rc<MountedData>>);
    if !(owner.main)() {
        return rsx! {};
    }
    let failures = (owner.transfers)()
        .into_values()
        .flatten()
        .chain((owner.failure)())
        .collect::<Vec<_>>();
    rsx! {
        div { class: "flex flex-wrap items-center gap-2", "data-omp-attachments": "true",
            IconButton { icon: IconType::Paperclip, label: "Attach files", title: "Attach files", variant: ButtonVariant::Ghost, size: ButtonSize::IconSm,
                "aria-disabled": disabled.then_some("true"),
                onmousedown: move |event: MouseEvent| event.prevent_default(),
                onclick: move |event: MouseEvent| {
                    event.prevent_default();
                    if !disabled && let Some(input) = input.peek().as_ref() { upload_dom::open_file_chooser(input); }
                },
            }
            AttachmentInput { on_chosen: owner.chosen, on_reading: Some(owner.reading), onmounted: Some(EventHandler::new(move |event: MountedEvent| input.set(Some(event.data())))) }
            if !failures.is_empty() {
                div { role: "alert", class: "flex flex-wrap items-center gap-2 text-sm text-destructive",
                    for message in failures { p { "{message}" } }
                    Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm, onclick: move |_| owner.discard_failed.call(()), "Discard failed uploads" }
                }
            }
        }
    }
}

fn fence(upload_id: &str) -> String {
    format!("omp-attachment:{upload_id}")
}

fn attachment_input(path: &str) -> Result<String, &'static str> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(
            "The uploaded path contains unsupported control characters and was not sent to OMP.",
        );
    }
    Ok(format!("\u{1b}[200~{path}\u{1b}[201~"))
}

#[cfg(test)]
mod tests {
    use super::attachment_input;
    #[test]
    fn worker_paths_preserve_spaces_unicode_and_quotes_without_shell_rewriting() {
        let path = "/tmp/Team's work/日本語 😀.png";
        assert_eq!(
            attachment_input(path).unwrap(),
            format!("\u{1b}[200~{path}\u{1b}[201~")
        );
    }
    #[test]
    fn worker_control_bytes_cannot_end_the_paste_or_inject_keys() {
        for path in [
            "",
            "/tmp/a\n.png",
            "/tmp/a\u{1b}[201~\r.png",
            "/tmp/a\0.png",
        ] {
            assert!(attachment_input(path).is_err());
        }
    }
}
