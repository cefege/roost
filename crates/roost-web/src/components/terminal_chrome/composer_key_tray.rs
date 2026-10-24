//! The TV input tray's half inside the composer bar: the keys toggle, and the
//! terminal keys drawn above the bar while open. The pane hands the keys in
//! (`CellTerminal`, which owns the key pad and its latches); the composer only
//! places them, so text, mic, Send and keys are one surface on a television.

use dioxus::prelude::*;

use crate::components::terminal::terminal_nav_pad::{key_toggle_icon, key_toggle_label};
use libero::components::ActionIcon;

/// The terminal keys a pane hands its composer, and whether they show.
#[derive(Clone, PartialEq)]
pub struct KeyTray {
    /// The store's key-pad flag: the same one the controller's Select toggles.
    pub open: bool,
    /// The key grid, drawn above the bar while `open`.
    pub keys: Element,
    /// Open or close the tray.
    pub on_toggle: EventHandler<()>,
}

impl std::fmt::Debug for KeyTray {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyTray")
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

/// The tray's keys (while open) and its toggle, as children of the bar.
#[component]
pub fn ComposerKeyTray(tray: KeyTray) -> Element {
    let open = tray.open;
    let on_toggle = tray.on_toggle;
    let control_sx = use_hook(|| {
        libero::sx::sx()
            .width("var(--term-chat-control)")
            .height("var(--term-chat-control)")
            .border_radius("9999rem")
    });
    rsx! {
        if open {
            {tray.keys.clone()}
        }
        ActionIcon {
            icon: key_toggle_icon(open),
            aria_label: key_toggle_label(open),
            variant: "standard",
            size: "lg",
            sx: control_sx,
            "data-testid": "chat-keys",
            "data-open": if open { "true" } else { "false" },
            onmousedown: |event: MouseEvent| event.prevent_default(),
            onclick: move |_| on_toggle.call(()),
        }
    }
}
