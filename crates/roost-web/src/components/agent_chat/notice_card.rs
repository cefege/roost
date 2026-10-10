//! A harness notice rendered as a Material card with level-specific tone.
//! Its body uses the same safe Markdown renderer as assistant messages.
//! Safety-sensitive Markdown is rendered through the transcript's sanitizer.

use dioxus::prelude::*;

use super::markdown::markdown_to_safe_html;
use crate::components::md::{Card, CardVariant};

#[component]
pub fn NoticeCard(id: String, level: String, title: String, body: String) -> Element {
    let rendered = markdown_to_safe_html(&body);
    let tone = match level.as_str() {
        "error" => "error",
        "warn" | "warning" => "warning",
        _ => "info",
    };
    rsx! {
        Card {
            key: "{id}",
            title: Some(title),
            variant: CardVariant::Outlined,
            class: format!("agent-chat__notice-card agent-chat__notice-card--{tone}"),
            children: rsx! { div { class: "agent-chat__markdown", dangerous_inner_html: rendered } },
        }
    }
}
