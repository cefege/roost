//! The shared Rust/UI InputPrompt reference on the design gallery.
//! The terminal and agent surfaces use this same frame, field and toolbar.
//! This specimen owns only local draft state; it makes no backend requests.

use dioxus::prelude::*;

use crate::components::ui::{
    Icon, IconType, InputPrompt, InputPromptFooter, InputPromptSubmit, InputPromptTextarea,
    InputPromptTools,
};

#[component]
pub(super) fn ComposerSpecimen() -> Element {
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
        div { class: "flex min-w-0 max-w-2xl flex-col gap-3",
            p { class: "text-sm text-muted-foreground",
                "Terminal and agent chat share this frame, textarea and toolbar. Each mode supplies its own tools."
            }
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
}
