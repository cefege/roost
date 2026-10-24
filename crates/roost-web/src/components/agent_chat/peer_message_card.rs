//! A message a peer conversation sent this one, as its own transcript row. It
//! reads as an unprompted arrival rather than a tool result, so it uses the
//! `Card` primitive and plain text: peer bodies are not trusted Markdown. The
//! transcript mounts it under the item's durable id.

use dioxus::prelude::*;

use crate::components::ui::{Card, CardContent, CardHeader, CardTitle};

#[component]
pub fn PeerMessageCard(id: String, from: String, broadcast: bool, text: String) -> Element {
    let title = if broadcast {
        format!("Broadcast from {from}")
    } else {
        format!("Message from {from}")
    };
    rsx! {
        Card {
            key: "{id}",
            CardHeader {
                CardTitle { "{title}" }
            }
            CardContent {
                div { class: "text-sm whitespace-pre-wrap wrap-break-word text-foreground", "{text}" }
            }
        }
    }
}
