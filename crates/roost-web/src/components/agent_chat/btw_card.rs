//! A side-question's pending, answered or failed transcript card.
//! Transcript projection supplies canonical state; this view owns no request.
//! Markdown and Libero primitives preserve semantic output and loading feedback.

use crate::components::ui::Markdown;
use dioxus::prelude::*;
use libero::{
    components::{Flex, Loader, Paper, Text},
    sx::sx,
};

#[component]
pub(super) fn BtwCard(question: String, answer: Option<String>, error: Option<String>) -> Element {
    let card_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("md"));
    let title_sx = use_hook(|| sx().font_weight("600"));
    let answer_sx = use_hook(|| sx().color("ink").with("overflow-wrap", "anywhere"));
    rsx! {
        Paper { sx: card_sx,
            Text { size: "lg", sx: title_sx, "Side question" }
            Flex { direction: "column", gap: "sm",
                Text { size: "sm", "{question}" }
                if let Some(answer) = answer {
                    Markdown { sx: answer_sx, text: answer }
                } else if let Some(error) = error {
                    Text { size: "sm", color: "error", role: "alert", "{error}" }
                } else {
                    Flex { direction: "row", align: "center", gap: "sm", role: "status",
                        Loader { size: "sm", aria_hidden: "true" }
                        Text { color: "muted", "Finding an answer…" }
                    }
                }
            }
        }
    }
}
