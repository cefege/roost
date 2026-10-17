//! The agent UI's one alert block — a bordered `Surface` with a tone icon
//! beside its message — used by the transcript, the composer's pinned run
//! error and the agent settings panes. An error under a control is a line of
//! `ERROR_TEXT_CLASS` text instead, as in the other settings panes.

use dioxus::prelude::*;

use crate::components::ui::{Icon, IconType, Surface, SurfaceRadius};

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
    let (icon, role, icon_class) = match tone {
        AlertTone::Error => (IconType::CircleAlert, "alert", "text-destructive"),
        AlertTone::Warning => (IconType::TriangleAlert, "status", "text-warning"),
    };
    rsx! {
        Surface {
            role: Some(role.to_owned()),
            class: Some("flex items-start gap-2 text-sm text-foreground".to_owned()),
            border: true,
            radius: SurfaceRadius::Md,
            pad: Some(3),
            test_id,
            Icon { icon, class: icon_class }
            div { class: "min-w-0 flex-1", {children} }
        }
    }
}
