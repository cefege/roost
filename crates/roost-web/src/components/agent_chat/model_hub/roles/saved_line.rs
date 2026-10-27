//! Direct entry points to the all-folders Default and Fast models.
//! The hub supplies human-readable model labels; this row only selects a role.
//! Each button opens that role's picker without changing the chat's model pin.

use crate::components::ui::{Button, ButtonSize, ButtonVariant};
use dioxus::prelude::*;

#[component]
pub fn SavedRolesLine(
    default_model: String,
    fast_model: String,
    on_open: EventHandler<&'static str>,
) -> Element {
    rsx! {
        div { class: "flex shrink-0 flex-wrap items-center gap-2 text-sm", "data-testid": "agent-model-hub-saved-roles",
            span { class: "text-muted-foreground", "Saved for new chats:" }
            Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm, class: "max-w-full", title: "Default model: {default_model}", "data-testid": "agent-model-hub-saved-main",
                onclick: move |_| on_open.call("default"),
                span { class: "min-w-0 truncate", "Default: {default_model}" }
            }
            Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm, class: "max-w-full", title: "Fast model for searches and side questions: {fast_model}", "data-testid": "agent-model-hub-saved-fast",
                onclick: move |_| on_open.call("smol"),
                span { class: "min-w-0 truncate", "Fast: {fast_model}" }
            }
        }
    }
}
