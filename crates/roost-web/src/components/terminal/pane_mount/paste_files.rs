//! File paste into a terminal pane: the clipboard files a paste event carried,
//! handed to the same upload path a drop takes. Called by `pane_mount::input`'s
//! paste handler; the upload is `terminal_chrome::terminal_upload`'s.

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

use super::PaneShared;

/// Upload the files `event` carried, the way a drop uploads them, and answer
/// whether there were any.
#[cfg(target_arch = "wasm32")]
pub(super) fn upload_pasted_files(shared: &PaneShared, event: &web_sys::ClipboardEvent) -> bool {
    let Some(file_list) = event.clipboard_data().and_then(|data| data.files()) else {
        return false;
    };
    let files: Vec<web_sys::File> = (0..file_list.length())
        .filter_map(|index| file_list.get(index))
        .collect();
    if files.is_empty() {
        return false;
    }
    let pane = shared.weak_self();
    let type_raw: Rc<dyn Fn(&str)> = Rc::new(move |text: &str| {
        if let Some(pane) = pane.upgrade() {
            super::input::send_bytes(&pane, text.as_bytes().to_vec(), false);
        }
    });
    crate::components::terminal_chrome::terminal_upload::upload_pasted_files(
        &shared.pump,
        &shared.session_id,
        &shared.worker_fp,
        files,
        type_raw,
    );
    true
}

/// No clipboard files outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn upload_pasted_files(_shared: &PaneShared, _event: &web_sys::ClipboardEvent) -> bool {
    false
}
