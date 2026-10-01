//! The overlay host: the ⌘K sheet, and the Start-button controller map beside
//! it. Ports `apps/web/src/components/palette/CommandPalette.tsx` and the
//! mounting of `ControllerMap` from `App.tsx`; the body is `palette::body`.
//!
//! The host stays mounted and the body does not. That is the whole of v2's
//! perf note: the catalog build and the filter only exist while the palette is
//! open, so a WebSocket tick does nothing at all to a reader who is not looking
//! at it, and reopening starts from an empty query with the field focused.
//!
//! Mounted by `app::AuthorizedOverlays` beside the rename dialog and the
//! notification dock.

pub mod body;
pub mod controller_map;
pub mod dom;
pub mod list_keys;
pub mod outcome;
pub mod pieces;

use dioxus::prelude::*;

use self::body::PaletteBody;
use self::controller_map::ControllerMap;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{AutoFocusRequest, Sheet, SheetSide};

/// The command palette, and the controller map that shares its mount point.
#[component]
pub fn CommandPalette() -> Element {
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    let pump = crate::pump::use_store();
    let compact = use_is_compact();
    // Read THROUGH the signal, not `peek()`ed: `peek` is an explicit promise
    // not to subscribe, and this component's only job is to appear when that
    // signal turns true. Peeking rendered the sheet once with the closed state
    // and left it there, so both the chord and the pad's X button set the flag
    // and no dialog ever appeared.
    let open = overlays.palette.cloned();
    let side = if compact {
        SheetSide::Bottom
    } else {
        SheetSide::Center
    };
    let on_close = {
        let mut palette = overlays.palette;
        EventHandler::new(move |()| palette.set(false))
    };
    // The dialog's own auto-focus would land on the focus trap's first tabbable
    // element, which is the close button; the body owns where focus goes.
    let on_open_auto_focus =
        EventHandler::new(|request: AutoFocusRequest| request.prevent_default());
    rsx! {
        Sheet {
            open,
            on_close,
            headline: "Command palette",
            side,
            class: "roost-dialog--wide roost-dialog--command-palette",
            test_id: Some("command-palette".to_string()),
            on_open_auto_focus,
            if open {
                PaletteBody { pump, overlays }
            }
        }
        ControllerMap {}
    }
}
