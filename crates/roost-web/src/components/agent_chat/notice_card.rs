//! Durable harness notices retain severity and safe-Markdown details.
//! Fork notices replace embedded navigation with the native route action.
//! The transcript mounts each notice under its durable item identity.

use dioxus::prelude::*;

use super::markdown::markdown_to_safe_html;
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle, CardVariant, Collapsible,
    CollapsibleContent, CollapsibleTrigger, Icon, IconType, Markdown,
};
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
    let display_body = if fork_target.is_some() {
        body.rsplit_once('[')
            .map_or(body.as_str(), |(prefix, _)| prefix)
    } else {
        body.as_str()
    };
    let rendered = markdown_to_safe_html(display_body);
    let long_body = display_body.lines().count() > 3 || display_body.chars().count() > 360;
    let mut details_open = use_signal(|| false);
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
                    Collapsible {
                        class: "text-sm",
                        open: details_open(),
                        on_open_change: move |next: bool| details_open.set(next),
                        CollapsibleTrigger {
                            variant: ButtonVariant::Ghost,
                            class: "h-auto justify-start px-0 text-muted-foreground",
                            "Details"
                        }
                        CollapsibleContent {
                            Markdown { html: rendered }
                        }
                    }
                } else if !display_body.is_empty() {
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
