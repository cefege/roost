//! The touch/controller terminal key sheet: the key grid (`terminal_nav_keys`)
//! as a floating sheet, plus the fixed toggle that opens it, and the toggle's
//! face, which the TV input tray's toggle shares.
//!
//! Renders as body-level fixed overlays, OUTSIDE the composer dock's subtree,
//! so opening it moves neither the dock nor the terminal. Ports
//! `apps/web/src/components/terminal/TerminalNavButtons.tsx`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::terminal_nav_pad::terminal_nav_pad_open;

use super::pane_handle::PaneHandle;
use super::pane_state::PaneUi;
use super::terminal_nav_keys::TerminalKeyGrid;
use crate::components::layout::portal::Portal;
use crate::components::md::{ButtonVariant, IconButton, IconButtonSize};
use crate::pump::Pump;

#[component]
pub fn TerminalNavPad(
    handle: PaneHandle,
    ui: PaneUi,
    pump: Pump,
    on_ctrl_armed: EventHandler<bool>,
    on_link_armed: EventHandler<bool>,
) -> Element {
    let revision = pump.revision();
    let _ = revision.read();
    let open = terminal_nav_pad_open(pump.core().borrow().store());
    let toggle_pump = pump.clone();
    let toggle = move |_event: MouseEvent| {
        toggle_pump.dispatch(ClientEvent::Shell(ShellIntent::ToggleNavPad));
    };

    rsx! {
        Portal {
            if open {
                TerminalKeyGrid {
                    handle,
                    ui,
                    pump: pump.clone(),
                    on_ctrl_armed,
                    on_link_armed,
                    placement: "sheet",
                }
            }
            IconButton {
                icon: key_toggle_icon(open),
                label: key_toggle_label(open),
                variant: ButtonVariant::Ghost,
                size: IconButtonSize::IconLg,
                class: Some("term-nav-toggle".to_owned()),
                "data-testid": "terminal-nav-toggle",
                "data-open": if open { "true" } else { "false" },
                // Every press here is one tap into a pane the reader must not be
                // pulled out of.
                onmousedown: |event: MouseEvent| event.prevent_default(),
                onclick: toggle,
            }
        }
    }
}

/// The toggle's glyph: a chevron while the keys are up, the keyboard while
/// they are not, so the control states what one press will do.
pub fn key_toggle_icon(open: bool) -> String {
    (if open {
        "keyboard_arrow_down"
    } else {
        "keyboard"
    })
    .to_owned()
}

/// The toggle's accessible name.
pub fn key_toggle_label(open: bool) -> String {
    (if open {
        "Hide terminal keys"
    } else {
        "Show terminal keys"
    })
    .to_owned()
}
