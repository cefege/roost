//! Files into a terminal pane: the one upload context a pane's attach button,
//! file drop, and file paste build, and the typed-path sink they share. Called
//! by `terminal::cell_terminal` (picker and drop) and `terminal::pane_mount`
//! (paste); the queue and route plan are `super::upload`'s.

use dioxus::prelude::{EventHandler, Signal, WritableExt};
use std::rc::Rc;

use super::attachment_picker::ChosenFile;
use super::short_paths::short_path_preference;
use super::upload::{UploadContext, enqueue_attachments};
use super::upload_host::DirectIdentity;
use crate::components::terminal::pane_handle::PaneHandle;
use crate::pump::Pump;

/// Upload `chosen` to the session's worker and type each committed path into
/// the pane through `type_raw`.
///
/// The tab and device are read once per gesture, so every file of it is bound
/// to the same identity; what the tab can reach is read per upload. The path
/// gets a trailing space so the next keystroke starts a new word, and is typed
/// raw, not bracketed: it is a word of the command line, not pasted content.
pub fn upload_into_terminal(
    pump: &Pump,
    session_id: &str,
    worker_fp: &str,
    chosen: Vec<ChosenFile>,
    type_raw: Rc<dyn Fn(&str)>,
) {
    if chosen.is_empty() {
        return;
    }
    let tab_id = pump.core().borrow().store().tab_id.clone();
    let device_fingerprint = pump
        .rpc()
        .device_key()
        .map(|key| key.fingerprint().to_owned())
        .unwrap_or_default();
    let context = UploadContext {
        session_id: session_id.to_owned(),
        worker_fp: Some(worker_fp.to_owned()),
        short_path: short_path_preference(),
        identity: DirectIdentity {
            tab_id,
            device_fingerprint,
        },
    };
    let sink: Rc<dyn Fn(&str)> = Rc::new(move |quoted: &str| type_raw(&format!("{quoted} ")));
    enqueue_attachments(pump, &context, chosen, sink);
}
/// Append an entry-point's files to the pane's staged set.
pub fn stage_attachments(
    staged_files: Signal<Vec<ChosenFile>>,
    session_id: String,
) -> EventHandler<Vec<ChosenFile>> {
    EventHandler::new(move |chosen: Vec<ChosenFile>| {
        tracing::info!(
            target: "attachments",
            session = %session_id,
            files = chosen.len(),
            "terminal attachments staged"
        );
        let mut staged_files = staged_files;
        staged_files.with_mut(|staged| staged.extend(chosen));
    })
}

/// Bind confirmed uploads to the pane whose composer owns them.
pub fn send_attachments(
    pump: Pump,
    session_id: String,
    worker_fp: String,
    handle: PaneHandle,
) -> EventHandler<Vec<ChosenFile>> {
    EventHandler::new(move |chosen| {
        let sink_handle = handle.clone();
        let type_raw: Rc<dyn Fn(&str)> = Rc::new(move |text: &str| sink_handle.send_raw_text(text));
        upload_into_terminal(&pump, &session_id, &worker_fp, chosen, type_raw);
    })
}

/// Read pasted files and stage them in this pane's composer rather than
/// starting an upload before the operator confirms Send.
#[cfg(target_arch = "wasm32")]
pub fn stage_pasted_files(mut staged_files: Signal<Vec<ChosenFile>>, files: Vec<web_sys::File>) {
    wasm_bindgen_futures::spawn_local(async move {
        let chosen = super::attachment_picker::read_chosen(files).await;
        staged_files.with_mut(|staged| staged.extend(chosen));
    });
}
