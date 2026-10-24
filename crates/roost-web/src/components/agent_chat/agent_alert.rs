//! A Libero alert with an icon for agent errors and warnings.
//! Transcript, composer status, tool cards and settings panes use this notice.
//! Inline control errors use the same semantic Libero text role directly.
use dioxus::prelude::*;
use libero::{
    components::{Alert, Pictogram},
    sx::sx,
};

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
    let (icon, color) = match tone {
        AlertTone::Error => (pictogram_icons_lucide::circle_alert::outlined, "error"),
        AlertTone::Warning => (pictogram_icons_lucide::triangle_alert::outlined, "warning"),
    };
    let alert_sx = use_hook(|| sx().gap("sm").align_items("center").padding("sm"));
    rsx! {
        Alert { color, sx: alert_sx,
            icon: rsx! { Pictogram { icon, width: "16", height: "16" } },
            "data-testid": test_id,
            {children}
        }
    }
}
