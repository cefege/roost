//! Passive selected-terminal carrier label for desktop and compact terminal
//! headers. DECK's pane strip and mobile bar supply the session; the store's
//! `terminal_transport` selector is the only source of baseline-qualified
//! carrier state. Ports `apps/web/src/components/terminal/TerminalTransportIndicator.tsx`.

use crate::pump::use_store;
use dioxus::prelude::*;
use libero::components::{Box, Text};
use libero::sx::sx;
use roost_client_core::store::terminal_transport::session_terminal_transport_presentation;

/// The chip.
#[component]
pub fn TerminalTransportIndicator(session_id: String) -> Element {
    let pump = use_store();
    let presentation = {
        let core = pump.core();
        let core = core.borrow();
        session_terminal_transport_presentation(core.store(), &session_id)
    };
    let indicator_sx = use_hook(|| {
        sx().display("inline-flex")
            .min_width("0")
            .flex("0 1 auto")
            .align_items("center")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .white_space("nowrap")
            .selector(
                "& > *",
                sx().min_height("0")
                    .padding_inline("sm")
                    .padding_block("xs")
                    .font_size("xs")
                    .color("muted"),
            )
    });
    rsx! {
        Box {
            component: "div",
            sx: indicator_sx,
            "data-testid": "terminal-transport-indicator",
            "data-session-id": "{session_id}",
            "data-terminal-transport": presentation.kind_attribute(),
            role: "group",
            "aria-label": "Terminal transport",
            Text { size: "xs", color: "text-dimmed", title: "{presentation.description}", "{presentation.label}" }
        }
    }
}
