//! A peer conversation's message as its own transcript row. It reads as an
//! unprompted arrival rather than a tool result, so it uses a Libero `Paper`
//! and plain text: peer bodies are not trusted Markdown. The transcript mounts
//! it under the item's durable id.

use dioxus::prelude::*;
use libero::{
    components::{Flex, Paper, Text, Title},
    sx::sx,
};

#[component]
pub fn PeerMessageCard(id: String, from: String, broadcast: bool, text: String) -> Element {
    let card = use_hook(|| sx().gap("sm").padding("sm").width("100%"));
    let body = use_hook(|| sx().white_space("pre-wrap").overflow_wrap("anywhere"));
    let title = if broadcast {
        format!("Broadcast from {from}")
    } else {
        format!("Message from {from}")
    };
    rsx! {
        Paper { key: "{id}", sx: card,
            Flex { direction: "column", gap: "sm",
                Title { size: "sm", "{title}" }
                Text { sx: body, "{text}" }
            }
        }
    }
}
