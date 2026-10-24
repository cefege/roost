//! Compact advisor feedback with a severity role and delivery state.
//! Displayed inline in the transcript after the primary turn.
//! The harness retains delivery state; this card only presents it.

use dioxus::prelude::*;

use crate::components::ui::{Card, CardContent, CardSize, Icon, IconType};

#[component]
pub fn AdvisoryCard(id: String, severity: String, note: String, delivered: bool) -> Element {
    let (label, tone_class, icon) = match severity.as_str() {
        "blocker" => ("Blocker", "text-destructive", IconType::CircleAlert),
        "concern" => ("Concern", "text-warning", IconType::TriangleAlert),
        _ => ("Nit", "text-info", IconType::Info),
    };
    rsx! {
        Card {
            key: "{id}",
            size: CardSize::Sm,
            CardContent { class: Some("flex flex-col gap-2".to_owned()),
                div { class: "flex items-center gap-2 font-medium {tone_class}",
                    Icon { icon, class: tone_class }
                    strong { "{label}" }
                }
                p { class: "m-0 text-sm text-foreground", "{note}" }
                if !delivered {
                    p { class: "m-0 text-sm text-muted-foreground", "Not delivered yet" }
                }
            }
        }
    }
}
