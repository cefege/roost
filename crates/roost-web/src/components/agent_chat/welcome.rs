//! A conversation with nothing in it yet: a greeting that says where the
//! agent's tools run, the composer at the centre of the pane, and a few
//! starting points that fill the draft. Rendered by `agent_chat::surface`
//! until the first message; the composer then docks at the bottom.

use dioxus::prelude::*;

use crate::components::md::Chip;

/// Starting points: (chip label, the draft it writes).
const SUGGESTIONS: [(&str, &str); 4] = [
    (
        "Explain this project",
        "Explain how this project is structured and where the main entry points are.",
    ),
    (
        "Review my changes",
        "Review my uncommitted changes and point out anything risky.",
    ),
    (
        "Fix a failing test",
        "Run the tests, find one that fails, and fix it.",
    ),
    (
        "Recent history",
        "Summarize what changed in the last few commits.",
    ),
];

#[component]
pub fn AgentWelcome(
    folder: String,
    machine: String,
    on_suggest: EventHandler<String>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "agent-chat__welcome",
            div { class: "agent-chat__welcome-inner",
                h2 { class: "agent-chat__welcome-title", "What should we work on in {folder}?" }
                p { class: "agent-chat__welcome-hint", "Tools run on {machine}, starting in this folder." }
                {children}
                div { class: "agent-chat__suggestions",
                    for (label, draft) in SUGGESTIONS {
                        Chip {
                            key: "{label}",
                            label: label.to_string(),
                            onclick: move |()| on_suggest.call(draft.to_string()),
                        }
                    }
                }
            }
        }
    }
}
