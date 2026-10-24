//! The unseen-bell mark: a small bell beside a session's tab title and sidebar
//! row while that session has rung since it was last on screen. Reads
//! `Store::terminal_bells`; `notifications::terminal_bell` clears the mark once
//! the session is visible. Rendered by `PaneTab` and `SessionRowFlat`.

use dioxus::prelude::*;
use libero::components::{Box, Pictogram};
use libero::sx::sx;

use crate::pump::use_store;

/// The bell, or nothing when `session_id` has no unseen ring.
#[component]
pub fn TerminalBellMark(session_id: String) -> Element {
    let mark_sx = use_hook(|| {
        sx().display("inline-flex")
            .flex_shrink("0")
            .color("primary")
    });
    let pump = use_store();
    let _ = pump.revision().read();
    let unseen = pump
        .core()
        .borrow()
        .store()
        .terminal_bells
        .is_unseen(&session_id);
    if !unseen {
        return rsx! {};
    }
    rsx! {
        Box {
            component: "span",
            sx: mark_sx,
            "data-testid": "terminal-bell-{session_id}",
            role: "img",
            "aria-label": "Bell rang",
            title: "Bell rang",
            Pictogram { icon: pictogram_icons_lucide::bell_ring::outlined, width: "0.75rem", height: "0.75rem" }
        }
    }
}
