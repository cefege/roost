//! First-conversation guidance and the upstream directory-exploration preset.
//! Owned separately from the transcript so draft edits only repaint the starter.
//! The action fills and focuses the native composer without submitting work.

use dioxus::prelude::*;

use super::composer_helpers::composer_input_id;
use crate::components::ui::anchored_panel::focus_element_by_id;
use crate::components::ui::{Button, ButtonVariant, EmptyState, IconType};

#[component]
pub(super) fn AgentWelcome(
    conversation_id: String,
    mut draft: Signal<String>,
    connected: bool,
) -> Element {
    rsx! {
        EmptyState {
            icon: IconType::Bot,
            title: "Your agent workspace is ready",
            supporting: "Start with a message or inspect this directory. Closing this view does not stop an active run.",
            action: rsx! {
                Button { variant: ButtonVariant::Outline, icon: Some(IconType::FolderOpen),
                    disabled: !connected || !draft.read().is_empty(),
                    onclick: move |_| {
                        draft.set("Inspect this directory using your tools and give me a brief overview. Do not modify any files.".to_owned());
                        focus_element_by_id(&composer_input_id(&conversation_id));
                    },
                    "Explore this directory"
                }
            },
        }
    }
}
