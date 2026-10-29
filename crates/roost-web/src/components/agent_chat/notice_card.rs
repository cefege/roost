//! Informational command notices use the transcript's compact Marker recipe.
//! Warnings and errors retain Alert severity and safe-Markdown details.
//! Fork notices keep their native route action; long details stay disclosed.

use dioxus::prelude::*;

use super::markdown::markdown_to_safe_html;
use crate::components::ui::{
    Alert, AlertTitle, AlertVariant, Button, ButtonSize, ButtonVariant, Collapsible,
    CollapsibleContent, CollapsibleTrigger, Icon, IconType, Markdown, Marker, MarkerContent,
    MarkerIcon,
};
use crate::router_state::use_navigate;

#[component]
pub fn NoticeCard(id: String, level: String, title: String, body: String) -> Element {
    let fork_target = (title == "Conversation forked")
        .then(|| fork_target(&body))
        .flatten();
    let navigate = use_navigate();
    let (severity, notice_icon, destructive) = match level.as_str() {
        "error" => ("Error", IconType::CircleAlert, true),
        "warn" | "warning" => ("Warning", IconType::TriangleAlert, false),
        _ => ("", IconType::Info, false),
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
    let details = rsx! {
        if long_body {
            Collapsible {
                open: details_open(),
                on_open_change: move |next: bool| details_open.set(next),
                CollapsibleTrigger {
                    size: ButtonSize::Sm,
                    Icon { icon: if details_open() { IconType::ChevronUp } else { IconType::ChevronDown } }
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
                size: ButtonSize::Sm,
                onclick: move |_| navigate.call(crate::routes::agent_href(&conversation_id)),
                "Open fork"
            }
        }
    };
    rsx! {
        div { key: "{id}", class: "min-w-0 py-1", "data-testid": "agent-notice", "data-level": level,
            if severity.is_empty() {
                Marker {
                    MarkerIcon { Icon { icon: notice_icon, class: "text-muted-foreground" } }
                    MarkerContent { class: "text-muted-foreground", "{title}" }
                }
                div { class: "pl-6", {details} }
            } else {
                Alert {
                    variant: if destructive { AlertVariant::Destructive } else { AlertVariant::Default },
                    class: "px-3 py-2 [&>svg]:left-3 [&>svg]:top-3 [&>svg~*]:pl-6",
                    Icon {
                        icon: notice_icon,
                        class: if destructive { "text-destructive" } else { "text-warning" },
                    }
                    AlertTitle { class: "mb-0", "{severity}: {title}" }
                    div { class: "mt-1", {details} }
                }
            }
        }
    }
}

fn fork_target(body: &str) -> Option<String> {
    let (_, suffix) = body.split_once("](/?conversation=")?;
    suffix.strip_suffix(')').map(str::to_owned)
}
