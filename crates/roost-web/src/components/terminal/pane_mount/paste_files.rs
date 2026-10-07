//! File paste into a terminal pane: clipboard files are staged in the pane's
//! composer rather than uploaded immediately.
//! Called by `pane_mount::input`; the picker supplies file previews.

use super::PaneShared;

/// Stage the files `event` carried, and answer whether it carried any.
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
    tracing::info!(
        target: "attachments",
        session = %shared.session_id,
        files = files.len(),
        "clipboard attachments staged"
    );
    crate::components::terminal_chrome::terminal_upload::stage_pasted_files(
        shared.staged_files,
        files,
    );
    true
}

/// No clipboard files outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn upload_pasted_files(_shared: &PaneShared, _event: &web_sys::ClipboardEvent) -> bool {
    false
}
