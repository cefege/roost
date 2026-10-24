//! Owns the file preview header: path, worker identity, byte size and download.
//! `FileViewer` supplies the settled path and current read metadata; this module
//! renders the header with Libero typography and layout primitives.

use dioxus::prelude::*;
use libero::components::{Flex, Text};
use libero::sx::sx;
use libero::theme::Size;

use crate::components::download::DownloadButton;

const FP_HEADER_CHARS: usize = 8;

#[component]
pub fn FileViewerHeader(
    file_path: String,
    worker_fp: String,
    downloadable: bool,
    byte_size: u64,
) -> Element {
    let header_sx = use_hook(|| sx().flex_shrink("0").align_items("center").gap("sm"));
    let path_sx = use_hook(|| {
        sx().flex("1")
            .min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .font_family("monospace")
            .font_size("sm")
            .color("muted")
    });
    let meta_sx = use_hook(|| {
        sx().flex_shrink("0")
            .white_space("nowrap")
            .font_family("monospace")
            .font_size("xs")
            .color("muted")
    });
    let fingerprint: String = worker_fp.chars().take(FP_HEADER_CHARS).collect();
    let size = format!("{byte_size} B");

    rsx! {
        Flex { sx: header_sx, direction: "row",
            Text {
                sx: path_sx,
                "data-testid": "file-viewer-sheet-title",
                "{file_path}"
            }
            Text {
                sx: meta_sx.clone(),
                "data-testid": "file-viewer-sheet-worker",
                "{fingerprint}"
            }
            if downloadable && byte_size > 0 {
                Text {
                    sx: meta_sx,
                    "data-testid": "file-viewer-sheet-size",
                    "{size}"
                }
            }
            if downloadable {
                DownloadButton { worker_fp, path: file_path, size: Size::Sm }
            }
        }
    }
}
