//! The touch/controller terminal key sheet: the key grid (`terminal_nav_keys`)
//! as a floating sheet, plus the fixed toggle that opens it, and the toggle's
//! face, which the TV input tray's toggle shares.
//!
//! Renders as body-level fixed overlays, OUTSIDE the composer dock's subtree,
//! so opening it moves neither the dock nor the terminal. Ports
//! `apps/web/src/components/terminal/TerminalNavButtons.tsx`.

use dioxus::prelude::*;
use libero::components::{ActionIcon, SvgData};
use libero::sx::sx;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::terminal_nav_pad::terminal_nav_pad_open;

use super::pane_handle::PaneHandle;
use super::pane_state::PaneUi;
use super::terminal_nav_keys::TerminalKeyGrid;
use crate::components::layout::portal::Portal;
use crate::pump::Pump;
use libero::components::Box;

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
    let toggle_sx = use_hook(|| {
        sx().position("fixed")
            .z_index("40")
            .with("right", "calc(env(safe-area-inset-right, 0px) + 1rem)")
            .with("bottom", "calc(env(safe-area-inset-bottom, 0px) + 9.5rem)")
            .selector(
                "html[data-tv='true'] &",
                sx().with("right", "calc(var(--tv-overscan-inline) + 1rem)")
                    .with("bottom", "calc(var(--tv-overscan-block) + 9.5rem)"),
            )
    });

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
            Box { component: "div", sx: toggle_sx,
                ActionIcon {
                    icon: key_toggle_icon(open),
                    aria_label: key_toggle_label(open),
                    variant: if open { "filled" } else { "outlined" },
                    color: if open { "secondary" } else { "primary" },
                    size: "lg",
                    selected: open,
                    "data-testid": "terminal-nav-toggle",
                    "data-open": if open { "true" } else { "false" },
                    onmousedown: |event: MouseEvent| event.prevent_default(),
                    onclick: toggle,
                }
            }
        }
    }
}

/// The toggle's glyph: a chevron while the keys are up, the keyboard while
/// they are not, so the control states what one press will do.
pub fn key_toggle_icon(open: bool) -> SvgData {
    if open {
        pictogram_icons_lucide::chevron_down::outlined
    } else {
        pictogram_icons_lucide::keyboard::outlined
    }
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
