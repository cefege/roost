//! Compact advisor feedback with a severity role and delivery state.
//! Displayed inline in the transcript after the primary turn.
//! The harness retains delivery state; this card only presents it.

use dioxus::prelude::*;

use crate::components::md::{Card, CardVariant, Icon, IconSize};

#[component]
pub fn AdvisoryCard(id: String, severity: String, note: String, delivered: bool) -> Element {
    let (label, tone) = match severity.as_str() {
        "blocker" => ("Blocker", "error"),
        "concern" => ("Concern", "warning"),
        _ => ("Nit", "info"),
    };
    rsx! {
        Card {
            key: "{id}",
            title: Some(label.to_string()),
            variant: CardVariant::Outlined,
            class: "agent-chat__advisory-card".to_string(),
            children: rsx! {
                div { class: "agent-chat__advisory-heading", "data-tone": tone,
                    Icon { name: if tone == "error" { "error" } else if tone == "warning" { "warning" } else { "info" }, size: IconSize::Sm }
                    strong { "{label}" }
                }
                p { "{note}" }
                if !delivered {
                    p { class: "agent-chat__advisory-pending", "Not delivered yet" }
                }
            },
        }
    }
}
