//! The jump-to-bottom button: a down arrow floated over the bottom edge of the
//! terminal display while the reader is scrolled back into history. Rendered by
//! `CellTerminal`, shown while `PaneUi::scrolled_back` is set; a press reaches
//! the pane through `PaneHandle::jump_to_live`.
//!
//! It sits in a zero-height anchor after the display, so it consumes no rows: a
//! control that took height would resize the PTY every time it appeared.

use dioxus::prelude::*;

use super::pane_handle::PaneHandle;
use crate::components::md::{ButtonVariant, IconButton};

/// The button, or nothing while the reader follows the live tail. `lift` is
/// the display's own `transform`, so the arrow rides a display a grown composer
/// pushed up instead of hiding under that composer.
#[component]
pub fn TerminalJumpToLive(visible: bool, handle: PaneHandle, lift: String) -> Element {
    if !visible {
        return rsx! {};
    }
    rsx! {
        div {
            class: "term-jump-anchor",
            style: "transform: {lift};",
            IconButton {
                icon: "arrow_downward",
                label: "Jump to the latest output",
                title: "Jump to the latest output",
                variant: ButtonVariant::Secondary,
                class: "term-jump-to-live",
                "data-testid": "terminal-jump-to-live",
                // Pressed without moving focus: the keyboard stays with the
                // terminal on a desktop, and no soft keyboard opens on a phone.
                onmousedown: move |event: MouseEvent| event.prevent_default(),
                onclick: move |_| handle.jump_to_live(),
            }
        }
    }
}
