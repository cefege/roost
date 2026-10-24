//! Every state the sheet shows that is not the file's text: the wait, the
//! machine that is not on this coordinator, the read that failed, the bytes
//! that are not text, the file that holds nothing, the file too large to paint,
//! and the path that is a folder. Called by `file_viewer`, which owns the
//! decision about which of them the answer was.
//!
//! Ports the named-state branches of
//! `apps/web/src/components/browse/FileViewerSheet.tsx`. Each one is a state a
//! reader can act on or read and leave, never a spinner that never resolves:
//! the sheet is the only surface in this product that can be certain the
//! coordinator will never answer.

use dioxus::prelude::*;
use libero::components::{Alert, Box, Button, Flex, Pictogram, Text};
use libero::sx::sx;

use super::state::MAX_VIEWED_FILE_BYTES;

/// The unavailable region's element id, so the sheet can put focus on it.
pub const UNAVAILABLE_REGION_ID: &str = "file-viewer-unavailable";

/// The name the unavailable region announces.
pub const UNAVAILABLE_LABEL: &str =
    "File unavailable. This file isn't available on this coordinator.";

/// The wait, while the machine's own registry has not answered yet.
#[component]
pub fn LoadingCaption() -> Element {
    let loading_sx = use_hook(|| sx().flex_shrink("0").font_size("sm").color("muted"));
    rsx! {
        Box {
            component: "span",
            sx: loading_sx,
            "data-testid": "file-viewer-sheet-loading",
            role: "status",
            "aria-live": "polite",
            "Loading file…"
        }
    }
}

/// A focusable denial state for a route whose worker is absent from the registry.
#[component]
pub fn UnavailableRegion(on_go_home: EventHandler<()>) -> Element {
    let scroll_region_sx = use_hook(|| sx().flex("1").min_height("0").overflow_y("auto"));
    let notice_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("center")
            .gap("sm")
            .padding_block("2rem")
            .text_align("center")
    });
    let icon_sx = use_hook(|| sx().width("2rem").height("2rem").color("muted"));
    rsx! {
        Box {
            sx: scroll_region_sx,
            id: UNAVAILABLE_REGION_ID,
            "data-testid": UNAVAILABLE_REGION_ID,
            role: "status",
            "aria-live": "polite",
            "aria-label": UNAVAILABLE_LABEL,
            "aria-atomic": "true",
            tabindex: "-1",
            Flex { direction: "column", sx: notice_sx,
                Box { component: "span", sx: icon_sx,
                    Pictogram { icon: pictogram_icons_lucide::file_x::outlined, width: "2rem", height: "2rem" }
                }
                Text { size: "lg", "File unavailable" }
                Text { size: "sm", color: "muted", "This file isn't available on this coordinator." }
                Button {
                    variant: "outlined",
                    "data-testid": "file-viewer-unavailable-home",
                    onfocus: move |_| super::dom::scroll_home_action_into_view(),
                    onclick: move |_| on_go_home.call(()),
                    "Go home"
                }
            }
        }
    }
}

/// Shows the worker's reason when the read cannot be completed.
#[component]
pub fn FailureNotice(message: String) -> Element {
    rsx! {
        Alert { color: "error", role: "status", "aria-live": "polite", "data-testid": "file-viewer-sheet-error", {message} }
    }
}

/// Describes binary content and its byte count.
#[component]
pub fn BinaryNotice(byte_size: u64) -> Element {
    let binary_sx = use_hook(|| sx().font_size("sm").color("warning"));
    rsx! {
        Box {
            component: "span",
            sx: binary_sx,
            "data-testid": "file-viewer-sheet-binary",
            role: "status",
            "aria-live": "polite",
            {format!("binary file ({byte_size} bytes) — not renderable as text")}
        }
    }
}

/// Explains that a successful read returned no content.
#[component]
pub fn EmptyNotice() -> Element {
    let notice_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("center")
            .gap("sm")
            .padding_block("2rem")
            .text_align("center")
    });
    let icon_sx = use_hook(|| sx().width("2rem").height("2rem").color("muted"));
    rsx! {
        Box { "data-testid": "file-viewer-sheet-empty",
            Flex { direction: "column", sx: notice_sx,
                Box { component: "span", sx: icon_sx,
                    Pictogram { icon: pictogram_icons_lucide::file::outlined, width: "2rem", height: "2rem" }
                }
                Text { size: "lg", "This file is empty" }
                Text { size: "sm", color: "muted", "There is nothing in it to read." }
            }
        }
    }
}

/// Reports the size that prevented the preview from being rendered.
#[component]
pub fn TooLargeNotice(byte_size: u64) -> Element {
    let notice_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("center")
            .gap("sm")
            .padding_block("2rem")
            .text_align("center")
    });
    let icon_sx = use_hook(|| sx().width("2rem").height("2rem").color("muted"));
    rsx! {
        Box { "data-testid": "file-viewer-sheet-too-large",
            Flex { direction: "column", sx: notice_sx,
                Box { component: "span", sx: icon_sx,
                    Pictogram { icon: pictogram_icons_lucide::file::outlined, width: "2rem", height: "2rem" }
                }
                Text { size: "lg", "This file is too large to open" }
                Text { size: "sm", color: "muted",
                    "It is {byte_size} bytes, and a preview reads up to {MAX_VIEWED_FILE_BYTES} bytes."
                }
            }
        }
    }
}

/// Explains that the route names a folder and offers navigation to the browser.
#[component]
pub fn DirectoryNotice(path: String, on_browse: EventHandler<()>) -> Element {
    let notice_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("center")
            .gap("sm")
            .padding_block("2rem")
            .text_align("center")
    });
    let icon_sx = use_hook(|| sx().width("2rem").height("2rem").color("muted"));
    rsx! {
        Box { "data-testid": "file-viewer-sheet-directory",
            Flex { direction: "column", sx: notice_sx,
                Box { component: "span", sx: icon_sx,
                    Pictogram { icon: pictogram_icons_lucide::folder::outlined, width: "2rem", height: "2rem" }
                }
                Text { size: "lg", "This is a folder" }
                Text { size: "sm", color: "muted", "{path} is a folder, not a file." }
                Button {
                    variant: "outlined",
                    "data-testid": "file-viewer-sheet-open-folder",
                    onclick: move |_| on_browse.call(()),
                    "Browse this folder"
                }
            }
        }
    }
}
