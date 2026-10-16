//! A message a peer conversation sent this one, as its own transcript row. It
//! reads as an unprompted arrival rather than a tool result, so it uses the
//! `Card` primitive and plain text: peer bodies are not trusted Markdown. The
//! transcript mounts it under the item's durable id.

use dioxus::prelude::*;

use crate::components::md::{Card, CardVariant};

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
            title: Some(title),
            variant: CardVariant::Outlined,
            class: "agent-chat__peer-message".to_string(),
            children: rsx! {
                div { class: "agent-chat__peer-message-body", "{text}" }
            },
        }
    }
}
