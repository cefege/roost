//! Durable harness notices retain severity and safe-Markdown details.
//! Fork notices replace embedded navigation with the native route action.
//! The transcript mounts each notice under its durable item identity.

use dioxus::prelude::*;
use libero::{
    components::{Badge, Button, Collapse, Flex, Paper, Pictogram, Title},
    sx::sx,
};

use crate::components::ui::Markdown;
use crate::router_state::use_navigate;

#[component]
pub fn NoticeCard(id: String, level: String, title: String, body: String) -> Element {
    let fork_target = (title == "Conversation forked")
        .then(|| fork_target(&body))
        .flatten();
    let navigate = use_navigate();
    let (severity, notice_icon, tone) = match level.as_str() {
        "error" => (
            "Error",
            pictogram_icons_lucide::circle_alert::outlined,
            "error",
        ),
        "warn" | "warning" => (
            "Warning",
            pictogram_icons_lucide::triangle_alert::outlined,
            "warning",
        ),
        _ => ("Notice", pictogram_icons_lucide::info::outlined, "info"),
    };
    let display_body = if fork_target.is_some() {
        body.rsplit_once('[')
            .map_or(body.as_str(), |(prefix, _)| prefix)
    } else {
        body.as_str()
    };
    let body_text = display_body.to_owned();
    let long_body = display_body.lines().count() > 3 || display_body.chars().count() > 360;
    let mut details_open = use_signal(|| false);
    let details_id = libero::hooks::use_id().read().clone();
    let card = use_hook(|| sx().gap("sm").padding("sm").width("100%"));
    let status_style = use_hook(|| sx().gap("xs").align_items("center"));
    rsx! {
        Paper { key: "{id}", sx: card,
            Flex { direction: "column", gap: "sm",
                Title { size: "sm", "{title}" }
                Flex { sx: status_style,
                    Pictogram { icon: notice_icon }
                    Badge {
                        color: if tone == "error" { "error" } else if tone == "warning" { "warning" } else { "info" },
                        variant: "tonal",
                        "{severity}"
                    }
                }
                if long_body {
                    Button {
                        variant: "standard",
                        color: "muted",
                        aria_expanded: details_open(),
                        aria_controls: details_id.clone(),
                        onclick: move |_| details_open.toggle(),
                        "Details"
                    }
                    Collapse { id: details_id, open: details_open(),
                        Markdown { text: body_text.clone() }
                    }
                } else if !display_body.is_empty() {
                    Markdown { text: body_text }
                }
                if let Some(conversation_id) = fork_target {
                    Button {
                        variant: "standard",
                        color: "primary",
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
