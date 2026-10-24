//! Compact advisor feedback with a severity role and delivery state.
//! Displayed inline in the transcript after the primary turn.
//! The harness retains delivery state; this card only presents it.

use dioxus::prelude::*;
use libero::{
    components::{Badge, Flex, Paper, Pictogram, Text},
    sx::sx,
};

#[component]
pub fn AdvisoryCard(id: String, severity: String, note: String, delivered: bool) -> Element {
    let layout = use_hook(|| sx().gap("sm").padding("sm").width("100%"));
    let heading = use_hook(|| sx().gap("xs").align_items("center"));
    let note_style = use_hook(|| sx().font_size("sm"));
    let delivery = use_hook(|| sx().font_size("sm").color("muted"));
    let (label, color, icon) = match severity.as_str() {
        "blocker" => (
            "Blocker",
            "error",
            pictogram_icons_lucide::circle_alert::outlined,
        ),
        "concern" => (
            "Concern",
            "warning",
            pictogram_icons_lucide::triangle_alert::outlined,
        ),
        _ => ("Nit", "info", pictogram_icons_lucide::info::outlined),
    };
    rsx! {
        Paper { key: "{id}", sx: layout,
            Flex { direction: "column", gap: "sm",
                Flex { sx: heading,
                    Pictogram { icon, width: "16", height: "16" }
                    Badge { color, variant: "tonal", "{label}" }
                }
                Text { sx: note_style, "{note}" }
                if !delivered {
                    Text { sx: delivery, "Not delivered yet" }
                }
            }
        }
    }
}
