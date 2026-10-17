//! The agent UI's alert block: a `ui` `Alert` with a tone icon beside its
//! message, used by the transcript, the composer's pinned run error, tool cards
//! and the agent settings panes. An error under a control is a line of
//! `ERROR_TEXT_CLASS` text instead, as in the other settings panes.

use dioxus::prelude::*;

use crate::components::ui::{Alert, AlertVariant, Icon, IconType};

/// The class of an inline error line under a control or a command.
pub const ERROR_TEXT_CLASS: &str = "text-sm text-destructive";

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
    let (icon, variant, role, icon_class) = match tone {
        AlertTone::Error => (
            IconType::CircleAlert,
            AlertVariant::Destructive,
            "alert",
            "text-destructive",
        ),
        AlertTone::Warning => (
            IconType::TriangleAlert,
            AlertVariant::Warning,
            "status",
            "text-warning",
        ),
    };
    rsx! {
        Alert { variant, role: role.to_owned(), test_id,
            Icon { icon, class: icon_class }
            div { class: "min-w-0 flex-1 text-sm text-foreground", {children} }
        }
    }
}
