//! Files into a terminal pane: the one upload context a pane's attach button,
//! file drop, and file paste build, and the typed-path sink they share. Called
//! by `terminal::cell_terminal` (picker and drop) and `terminal::pane_mount`
//! (paste); the queue and route plan are `super::upload`'s.

use std::rc::Rc;

use super::attachment_picker::ChosenFile;
use super::short_paths::short_path_preference;
use super::upload::{UploadContext, enqueue_attachments};
use super::upload_host::DirectIdentity;
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

/// Upload the files a paste carried, exactly as a drop is uploaded: the same
/// reader, then [`upload_into_terminal`].
#[cfg(target_arch = "wasm32")]
pub fn upload_pasted_files(
    pump: &Pump,
    session_id: &str,
    worker_fp: &str,
    files: Vec<web_sys::File>,
    type_raw: Rc<dyn Fn(&str)>,
) {
    tracing::info!(target: "upload", %session_id, files = files.len(), "files pasted into a terminal pane");
    let pump = pump.clone();
    let session_id = session_id.to_owned();
    let worker_fp = worker_fp.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        let chosen = super::attachment_picker::read_chosen(files).await;
        upload_into_terminal(&pump, &session_id, &worker_fp, chosen, type_raw);
    });
}
