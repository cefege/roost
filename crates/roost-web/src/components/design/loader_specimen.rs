//! `LoaderSpecimen`: the braille `Spinner` and the live `WorkingRow` the chat
//! draws under a running turn, rendered from the shipped components so the
//! gallery shows their real markup rather than a copy. Spacing comes from tokens.

use dioxus::prelude::*;

use crate::components::agent_chat::working_row::WorkingRow;
use crate::components::md::Spinner;

/// The loader frame beside its label, then the working row with a sample intent.
#[component]
pub fn LoaderSpecimen() -> Element {
    rsx! {
        div { style: "display: flex; flex-direction: column; gap: var(--md-space-3);",
            div { style: "display: flex; align-items: center; gap: var(--md-space-2);",
                Spinner { frame: '⠋' }
                span { style: "color: var(--text-mid);", "Spinner frame" }
            }
            WorkingRow { label: "Reading config".to_string(), paused: false }
        }
    }
}
