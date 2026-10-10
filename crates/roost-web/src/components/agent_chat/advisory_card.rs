//! Compact advisor feedback with a severity role and delivery state.
//! Displayed inline in the transcript after the primary turn.
//! The harness retains delivery state; this card only presents it.

use dioxus::prelude::*;

use crate::components::md::{Card, CardVariant};

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
            class: format!("agent-chat__advisory-card agent-chat__advisory-card--{tone}"),
            children: rsx! {
                p { "{note}" }
                if !delivered {
                    p { class: "agent-chat__advisory-pending", "Not delivered yet" }
                }
            },
        }
    }
}
