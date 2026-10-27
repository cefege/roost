//! The empty conversation's quiet prompt, rendered by the chat surface.
//! Uses the stock Empty header without a bordered card or suggested actions.
//! The composer remains the only primary action.

use crate::components::ui::{EmptyHeader, EmptyTitle};
use dioxus::prelude::*;

#[component]
pub(super) fn AgentWelcome() -> Element {
    rsx! {
        EmptyHeader { class: "py-12",
            EmptyTitle { "What would you like to work on?" }
        }
    }
}
