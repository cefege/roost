//! The TV key tray toggle in the terminal composer's footer.
//! `CellTerminal` owns the key grid and latches; `TerminalComposer` mounts
//! the supplied grid above the shared prompt, outside its clipping frame.

use dioxus::prelude::*;

use crate::components::terminal::terminal_nav_pad::{key_toggle_icon, key_toggle_label};
use crate::components::ui::{ButtonSize, ButtonVariant, IconButton};

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

/// The footer toggle; the composer mounts the supplied keys outside the frame.
#[component]
pub fn ComposerKeyTray(tray: KeyTray) -> Element {
    let open = tray.open;
    let on_toggle = tray.on_toggle;
    rsx! {
        IconButton {
            icon: key_toggle_icon(open),
            label: key_toggle_label(open),
            variant: ButtonVariant::Ghost,
            size: ButtonSize::IconSm,
            class: Some("tv:data-[open=true]:bg-accent tv:data-[open=true]:text-accent-foreground".to_owned()),
            "data-testid": "chat-keys",
            "data-open": if open { "true" } else { "false" },
            // A press must not pull focus off whatever the D-pad is on.
            onmousedown: |event: MouseEvent| event.prevent_default(),
            onclick: move |_| on_toggle.call(()),
        }
    }
}
