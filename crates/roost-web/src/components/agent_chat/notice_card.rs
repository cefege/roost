//! A harness notice rendered as a Material card with level-specific tone.
//! Its body uses the same safe Markdown renderer as assistant messages.
//! Safety-sensitive Markdown is rendered through the transcript's sanitizer.

use dioxus::prelude::*;

use super::markdown::markdown_to_safe_html;
use crate::components::md::{Button, ButtonVariant, Card, CardVariant};
use crate::router_state::use_navigate;

#[component]
pub fn NoticeCard(id: String, level: String, title: String, body: String) -> Element {
    let fork_target = (title == "Conversation forked")
        .then(|| fork_target(&body))
        .flatten();
    let navigate = use_navigate();
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
            children: rsx! {
                div { class: "agent-chat__markdown", dangerous_inner_html: rendered }
                if let Some(conversation_id) = fork_target {
                    Button {
                        variant: ButtonVariant::Link,
                        onclick: move |_| navigate.call(crate::routes::agent_href(&conversation_id)),
                        "Open fork"
                    }
                }
            },
        }
    }
}
fn fork_target(body: &str) -> Option<String> {
    let (_, suffix) = body.split_once("](/?conversation=")?;
    suffix.strip_suffix(')').map(str::to_owned)
}
