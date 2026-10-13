//! Durable harness notices retain severity and their complete safe-Markdown body.
//! Long details are disclosed explicitly rather than truncated or hidden behind hover.
//! The transcript mounts each notice under its durable item identity.

use dioxus::prelude::*;

use super::markdown::markdown_to_safe_html;
use crate::components::md::{Button, ButtonVariant, Card, CardVariant, Icon, IconSize};
use crate::router_state::use_navigate;

#[component]
pub fn NoticeCard(id: String, level: String, title: String, body: String) -> Element {
    let fork_target = (title == "Conversation forked")
        .then(|| fork_target(&body))
        .flatten();
    let navigate = use_navigate();
    let (severity, icon, tone) = match level.as_str() {
        "error" => ("Error", "error", "error"),
        "warn" | "warning" => ("Warning", "warning", "warning"),
        _ => ("Notice", "info", "info"),
    };
    let rendered = markdown_to_safe_html(&body);
    let long_body = body.lines().count() > 3 || body.chars().count() > 360;
    rsx! {
        Card {
            key: "{id}",
            title: Some(title),
            variant: CardVariant::Outlined,
            class: format!("agent-chat__notice-card agent-chat__notice-card--{tone}"),
            children: rsx! {
                div { class: "agent-chat__notice-heading", "data-tone": tone,
                    Icon { name: icon, size: IconSize::Sm }
                    strong { "{severity}" }
                }
                if long_body {
                    details { class: "agent-chat__notice-details",
                        summary { "Details" }
                        div { class: "agent-chat__markdown", dangerous_inner_html: rendered }
                    }
                } else if !body.is_empty() {
                    div { class: "agent-chat__markdown", dangerous_inner_html: rendered }
                }
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
