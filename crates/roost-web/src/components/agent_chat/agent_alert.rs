//! The agent UI's one alert block — a bordered `Surface` with a tone icon
//! beside its message — used by the transcript, the composer's pinned run
//! error and the agent settings panes. An error under a control is a line of
//! `ERROR_TEXT_CLASS` text instead, as in the other settings panes.

use dioxus::prelude::*;

use crate::components::md::{Icon, IconSize, Surface, SurfaceRadius};

/// The class of an inline error line under a control or a command.
pub const ERROR_TEXT_CLASS: &str = "agent-chat__error-text";

/// What an alert reports. An error interrupts the reader; a warning waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlertTone {
    #[default]
    Error,
    Warning,
}

#[component]
pub fn AgentAlert(
    #[props(default)] tone: AlertTone,
    test_id: Option<String>,
    children: Element,
) -> Element {
    let (icon, role, icon_class) = match tone {
        AlertTone::Error => (
            "error",
            "alert",
            "agent-chat__alert-icon agent-chat__alert-icon--error",
        ),
        AlertTone::Warning => (
            "warning",
            "status",
            "agent-chat__alert-icon agent-chat__alert-icon--warning",
        ),
    };
    rsx! {
        Surface { role, class: "agent-chat__alert", border: true, radius: SurfaceRadius::Md, pad: 3, test_id,
            Icon { name: icon, size: IconSize::Sm, class: icon_class }
            div { class: "agent-chat__alert-body", {children} }
        }
    }
}
