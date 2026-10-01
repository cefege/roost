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

use crate::components::md::{Button, ButtonVariant, EmptyState};

use super::state::MAX_VIEWED_FILE_BYTES;

/// The unavailable region's element id, so the sheet can put focus on it.
pub const UNAVAILABLE_REGION_ID: &str = "file-viewer-unavailable";

/// The name the unavailable region announces. v2 spells the sentence out in
/// one string, and so does this: it is what a screen reader says when the route
/// names a machine this coordinator has never heard of.
pub const UNAVAILABLE_LABEL: &str =
    "File unavailable. This file isn't available on this coordinator.";

/// The wait, while the machine's own registry has not answered yet. The
/// caption is the only thing on screen: a region that will be replaced by the
/// file or by the denial must not be announced twice.
#[component]
pub fn LoadingCaption() -> Element {
    rsx! {
        span {
            "data-testid": "file-viewer-sheet-loading",
            role: "status",
            "aria-live": "polite",
            style: "color: var(--text-lo); font-size: var(--md-body-s-size); flex: 0 0 auto;",
            "Loading file…"
        }
    }
}

/// The machine this file lives on is not in a registry that has already
/// published. Focusable on purpose: the route names a machine no registry row
/// will ever match, so a reader who lands here has to be able to leave with
/// the keyboard alone.
#[component]
pub fn UnavailableRegion(on_go_home: EventHandler<()>) -> Element {
    rsx! {
        div {
            id: UNAVAILABLE_REGION_ID,
            "data-testid": UNAVAILABLE_REGION_ID,
            role: "status",
            "aria-live": "polite",
            "aria-label": UNAVAILABLE_LABEL,
            "aria-atomic": "true",
            tabindex: "-1",
            style: "flex: 1 1 auto; min-height: 0; overflow-y: auto;",
            EmptyState {
                icon: "draft".to_owned(),
                title: "File unavailable".to_owned(),
                supporting: Some("This file isn't available on this coordinator.".to_owned()),
                action: rsx! {
                    Button {
                        variant: ButtonVariant::Secondary,
                        "data-testid": "file-viewer-unavailable-home",
                        onfocus: move |_| super::dom::scroll_home_action_into_view(),
                        onclick: move |_| on_go_home.call(()),
                        "Go home"
                    }
                },
            }
        }
    }
}

/// The read failed, and the machine named why: a permission, a path that is
/// not there, a machine that could not be reached. The machine's own words are
/// shown, because a reader who cannot open a file needs the reason, not a
/// paraphrase of it.
#[component]
pub fn FailureNotice(message: String) -> Element {
    rsx! {
        span {
            "data-testid": "file-viewer-sheet-error",
            role: "status",
            "aria-live": "polite",
            style: "color: var(--color-err); font-size: var(--md-body-s-size);",
            {message}
        }
    }
}

/// The bytes are not UTF-8. There is no text to number and no honest way to
/// show a few of it, so the sheet says what it is and how much of it there is.
#[component]
pub fn BinaryNotice(byte_size: u64) -> Element {
    rsx! {
        span {
            "data-testid": "file-viewer-sheet-binary",
            role: "status",
            "aria-live": "polite",
            style: "color: var(--color-warn); font-size: var(--md-body-s-size);",
            {format!("binary file ({byte_size} bytes) — not renderable as text")}
        }
    }
}

/// The file is there and holds nothing a reader could read. v2 painted an
/// empty body for it, which is indistinguishable from a failed read.
#[component]
pub fn EmptyNotice() -> Element {
    rsx! {
        div { "data-testid": "file-viewer-sheet-empty",
            EmptyState {
                icon: "insert_drive_file".to_owned(),
                title: "This file is empty".to_owned(),
                supporting: Some("There is nothing in it to read.".to_owned()),
            }
        }
    }
}

/// The file is larger than the sheet will paint. The size is named because
/// "too large" without a number is the least useful sentence in this product.
#[component]
pub fn TooLargeNotice(byte_size: u64) -> Element {
    rsx! {
        div { "data-testid": "file-viewer-sheet-too-large",
            EmptyState {
                icon: "insert_drive_file".to_owned(),
                title: "This file is too large to open".to_owned(),
                supporting: Some(format!(
                    "It is {byte_size} bytes, and a preview reads up to {MAX_VIEWED_FILE_BYTES} bytes."
                )),
            }
        }
    }
}

/// The path is a folder. The sheet cannot descend into it — a folder has no
/// lines — so it offers the browser, which is where a folder is actually read.
#[component]
pub fn DirectoryNotice(path: String, on_browse: EventHandler<()>) -> Element {
    rsx! {
        div { "data-testid": "file-viewer-sheet-directory",
            EmptyState {
                icon: "folder".to_owned(),
                title: "This is a folder".to_owned(),
                supporting: Some(format!("{path} is a folder, not a file.")),
                action: rsx! {
                    Button {
                        variant: ButtonVariant::Secondary,
                        "data-testid": "file-viewer-sheet-open-folder",
                        onclick: move |_| on_browse.call(()),
                        "Browse this folder"
                    }
                },
            }
        }
    }
}
