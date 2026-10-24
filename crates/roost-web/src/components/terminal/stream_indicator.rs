//! `StreamIndicator`: the terminal pane's stream-health dot, a `StatusDot` pinned
//! to the pane's top-right corner. `CellTerminal` and the `/design` gallery both
//! render it, so the specimen cannot drift from the pane. The wrapper is absolutely
//! positioned, so the dot never joins the pane's flex flow and never shrinks the
//! viewport.

use dioxus::prelude::*;

use crate::components::ui::StatusDot;

/// The dot's status role and whether it is hollow for a presentation state.
/// `detached` is the only remaining state, so it is the fallthrough arm.
fn stream_dot_style(state: &str) -> (&'static str, bool) {
    match state {
        "receiving" => ("info", false),
        "catching_up" => ("warn", false),
        _ => ("error", true),
    }
}

/// The indicator for one presentation state (`receiving`, `catching_up` or
/// `detached`), named by `title` on hover.
#[component]
pub fn StreamIndicator(state: &'static str, title: &'static str) -> Element {
    let (status, hollow) = stream_dot_style(state);
    rsx! {
        span {
            class: "pointer-events-none absolute top-2 right-2 z-2 flex",
            "data-testid": "terminal-stream-indicator",
            "data-state": state,
            title,
            "aria-hidden": "true",
            StatusDot { status: status.to_owned(), hollow }
        }
    }
}
