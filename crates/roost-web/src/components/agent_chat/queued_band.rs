//! Messages sent while the agent works, waiting for its next step, shown the
//! way OMP's queued band shows them: a muted count, then one line each in
//! order. Each moves into the conversation when the agent reads it.
//! Rendered by `transcript` at its foot, just above the composer.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::QueuedMessage;

#[component]
pub(super) fn QueuedBand(messages: Vec<QueuedMessage>) -> Element {
    if messages.is_empty() {
        return rsx! {};
    }
    let count = messages.len();
    rsx! {
        section { class: "agent-chat__queued", "aria-label": "Queued messages", "data-testid": "agent-chat-queued",
            p { class: "agent-chat__queued-head", "Queued · {count} · the agent reads them after its current step" }
            ol { class: "agent-chat__queued-list",
                for message in messages {
                    li { key: "{message.id}", class: "agent-chat__queued-row", title: message.text.clone(),
                        // One line each, as the terminal band shows them.
                        "{message.text.replace('\\n', \" ↵ \")}"
                    }
                }
            }
        }
    }
}
