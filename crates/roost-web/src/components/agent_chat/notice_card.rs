//! Durable harness notices retain severity and their complete safe-Markdown body.
//! Long details are disclosed explicitly rather than truncated or hidden behind hover.
//! The transcript mounts each notice under its durable item identity.

use dioxus::prelude::*;

use super::markdown::markdown_to_safe_html;
use crate::components::ui::{Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle, CardVariant, Icon, IconType, Markdown};
use crate::router_state::use_navigate;

#[component]
pub fn NoticeCard(id: String, level: String, title: String, body: String) -> Element {
    let fork_target = (title == "Conversation forked")
        .then(|| fork_target(&body))
        .flatten();
    let navigate = use_navigate();
    let (severity, notice_icon, tone) = match level.as_str() {
        "error" => ("Error", IconType::CircleAlert, "error"),
        "warn" | "warning" => ("Warning", IconType::TriangleAlert, "warning"),
        _ => ("Notice", IconType::Info, "info"),
    };
    let rendered = markdown_to_safe_html(&body);
    let long_body = body.lines().count() > 3 || body.chars().count() > 360;
    rsx! {
        Card {
            key: "{id}",
            variant: CardVariant::Outlined,
            CardHeader {
                CardTitle { "{title}" }
            }
            CardContent {
                div { class: "flex items-center gap-2 text-sm font-medium data-[tone=error]:text-destructive", "data-tone": tone,
                    Icon { icon: notice_icon, class: "size-4" }
                    strong { "{severity}" }
                }
                if long_body {
                    details { class: "text-sm",
                        summary { class: "cursor-pointer text-muted-foreground", "Details" }
                        Markdown { html: rendered }
                    }
                } else if !body.is_empty() {
                    Markdown { html: rendered }
                }
                if let Some(conversation_id) = fork_target {
                    Button {
                        variant: ButtonVariant::Link,
                        onclick: move |_| navigate.call(crate::routes::agent_href(&conversation_id)),
                        "Open fork"
                    }
                }
            }
        }
    }
}

fn fork_target(body: &str) -> Option<String> {
    let (_, suffix) = body.split_once("](/?conversation=")?;
    suffix.strip_suffix(')').map(str::to_owned)
}
