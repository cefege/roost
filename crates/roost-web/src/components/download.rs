//! The Download action: one icon button that pulls a worker file into the
//! browser's downloads. Placed by the file viewer's header and the browse
//! surface's file rows; the run itself is `download::host`'s.

pub mod host;

use dioxus::prelude::*;

use crate::components::ui::{ButtonSize, IconButton, IconType};
use crate::pump::use_store;

/// Download `path` from `worker_fp`. The transfer card on the dock reports the
/// run, with the coordinator route chip: chunk reads exist only there.
#[component]
pub fn DownloadButton(worker_fp: String, path: String, size: ButtonSize) -> Element {
    let pump = use_store();
    let label = match host::download_file_name(&path) {
        Some(name) => format!("Download {name}"),
        None => "Download".to_owned(),
    };
    let start = move |event: MouseEvent| {
        event.stop_propagation();
        host::start_download(&pump, &worker_fp, &path);
    };
    rsx! {
        IconButton {
            icon: IconType::Download,
            label,
            size,
            "data-testid": "file-download",
            onclick: start,
        }
    }
}
