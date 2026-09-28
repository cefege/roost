//! Passive selected-terminal carrier label for desktop and compact terminal
//! headers. DECK's pane strip and mobile bar supply the session; the store's
//! `terminal_transport` selector is the only source of baseline-qualified
//! carrier state. Ports `apps/web/src/components/terminal/TerminalTransportIndicator.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::terminal_transport::session_terminal_transport_presentation;

use crate::components::md::Chip;
use crate::pump::use_store;

/// The chip.
#[component]
pub fn TerminalTransportIndicator(session_id: String) -> Element {
    let pump = use_store();
    let presentation = {
        let core = pump.core();
        let core = core.borrow();
        session_terminal_transport_presentation(core.store(), &session_id)
    };
    rsx! {
        div {
            class: "terminal-transport-indicator",
            "data-testid": "terminal-transport-indicator",
            "data-session-id": "{session_id}",
            "data-terminal-transport": presentation.kind_attribute(),
            role: "group",
            "aria-label": "Terminal transport",
            Chip {
                label: presentation.label.to_owned(),
                title: presentation.description.to_owned(),
            }
        }
    }
}
