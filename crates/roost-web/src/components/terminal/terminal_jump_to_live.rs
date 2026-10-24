//! The jump-to-bottom button: a down arrow floated over the bottom edge of the
//! terminal display while the reader is scrolled back into history. Rendered by
//! `CellTerminal`, shown while `PaneUi::scrolled_back` is set; a press reaches
//! the pane through `PaneHandle::jump_to_live`.
//!
//! It sits in a zero-height anchor after the display, so it consumes no rows: a
//! control that took height would resize the PTY every time it appeared.

use super::pane_handle::PaneHandle;
use dioxus::prelude::*;
use libero::components::{ActionIcon, Box};
use libero::sx::sx;

/// The button, or nothing while the reader follows the live tail. `lift` is
/// the display's own `transform`, so the arrow rides a display a grown composer
/// pushed up instead of hiding under that composer.
#[component]
pub fn TerminalJumpToLive(visible: bool, handle: PaneHandle, lift: String) -> Element {
    let button_sx = use_hook(|| {
        sx().position("absolute")
            .inset_inline("0")
            .bottom("1rem")
            .margin_inline("auto")
    });
    let anchor_sx = use_hook(|| {
        sx().position("relative")
            .z_index("2")
            .height("0")
            .flex("none")
    });
    if !visible {
        return rsx! {};
    }
    rsx! {
        Box {
            component: "div",
            sx: anchor_sx,
            style: "transform: {lift};",
            ActionIcon {
                icon: pictogram_icons_lucide::arrow_down::outlined,
                aria_label: "Jump to the latest output",
                title: "Jump to the latest output",
                variant: "tonal",
                sx: button_sx,
                "data-testid": "terminal-jump-to-live",
                onmousedown: move |event: MouseEvent| event.prevent_default(),
                onclick: move |_| handle.jump_to_live(),
            }
        }
    }
}
