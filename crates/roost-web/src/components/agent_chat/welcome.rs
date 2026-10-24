//! First-conversation guidance and the upstream directory-exploration preset.
//! Owned separately from the transcript so draft edits only repaint the starter.
//! The action fills and focuses the native composer without submitting work.

use dioxus::prelude::*;
use libero::components::{Button, Flex, Paper, Pictogram, Text, Title};
use libero::sx::sx;

use super::composer_helpers::composer_input_id;

#[cfg(target_arch = "wasm32")]
fn focus_element_by_id(element_id: &str) {
    use wasm_bindgen::JsCast;

    if let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(element_id))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = element.focus();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn focus_element_by_id(_element_id: &str) {}

#[component]
pub(super) fn AgentWelcome(
    conversation_id: String,
    mut draft: Signal<String>,
    connected: bool,
) -> Element {
    let card_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("center")
            .gap("md")
            .padding("xl")
            .text_align("center")
    });
    let copy_sx = use_hook(|| sx().max_width("42ch"));
    rsx! {
        Paper { sx: card_sx, "data-testid": "agent-chat-welcome",
            Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "1.5rem", height: "1.5rem" }
            Title { size: "md", "Your agent workspace is ready" }
            Text { size: "sm", color: "muted", sx: copy_sx,
                "Start with a message or inspect this directory. Closing this view does not stop an active run."
            }
            Flex { direction: "row", gap: "sm",
                Button {
                    variant: "outlined",
                    icon: rsx! { Pictogram { icon: pictogram_icons_lucide::folder_open::outlined } },
                    disabled: !connected || !draft.read().is_empty(),
                    onclick: move |_| {
                        draft.set("Inspect this directory using your tools and give me a brief overview. Do not modify any files.".to_owned());
                        focus_element_by_id(&composer_input_id(&conversation_id));
                    },
                    "Explore this directory"
                }
            }
        }
    }
}
