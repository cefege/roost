//! The unseen-bell mark: a small bell beside a session's tab title and sidebar
//! row while that session has rung since it was last on screen. Reads
//! `Store::terminal_bells`; `notifications::terminal_bell` clears the mark once
//! the session is visible. Rendered by `PaneTab` and `SessionRowFlat`.

use dioxus::prelude::*;

use crate::components::md::{Icon, IconSize};
use crate::pump::use_store;

/// The bell, or nothing when `session_id` has no unseen ring.
#[component]
pub fn TerminalBellMark(session_id: String) -> Element {
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
        span {
            class: "terminal-bell-mark",
            "data-testid": "terminal-bell-{session_id}",
            role: "img",
            "aria-label": "Bell rang",
            title: "Bell rang",
            Icon { name: "notifications_active", size: IconSize::Sm }
        }
    }
}
