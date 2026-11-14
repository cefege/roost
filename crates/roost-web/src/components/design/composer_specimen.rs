//! The terminal composer reference on the design gallery.
//! The specimen owns local draft state and makes no backend requests.

use dioxus::prelude::*;

use crate::components::ui::{
    Icon, IconType, InputPrompt, InputPromptFooter, InputPromptSubmit, InputPromptTextarea,
    InputPromptTools,
};

#[component]
pub(super) fn ComposerSpecimen() -> Element {
    rsx! {
        div { class: "flex min-w-0 max-w-2xl flex-col gap-3",
            p { class: "text-sm text-muted-foreground",
                "The terminal composer uses this frame, textarea and toolbar."
            }
            ComposerFrameSpecimen {}
        }
    }
}

#[component]
fn ComposerFrameSpecimen() -> Element {
    let mut draft = use_signal(String::new);
    let mut submitted = use_signal(|| None::<String>);
    let submit = EventHandler::new(move |()| {
        let text = draft.peek().trim().to_owned();
        if !text.is_empty() {
            submitted.set(Some(text));
            draft.set(String::new());
        }
    });
    rsx! {
        InputPrompt {
            InputPromptTextarea {
                value: draft,
                aria_label: "Composer preview",
                on_submit: submit,
            }
            InputPromptFooter { class: "flex-wrap",
                InputPromptTools { class: "min-w-0 flex-1",
                    span { class: "text-xs text-muted-foreground", "Shift+Enter for a new line" }
                }
                InputPromptSubmit {
                    disabled: draft().trim().is_empty(),
                    aria_label: "Submit preview",
                    onclick: move |_| submit.call(()),
                    Icon { icon: IconType::ArrowUp, class: "size-4" }
                }
            }
        }
        if let Some(text) = submitted() {
            p { role: "status", class: "wrap-break-word text-sm text-muted-foreground",
                "Submitted locally: {text}"
            }
        }
    }
}
