//! The Copy affordance shared by native code, diff and terminal output blocks.
//! The button writes the caller's complete source, not the previewed rows.

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::ui::{Button, ButtonSize, ButtonVariant};
use dioxus::prelude::*;

#[component]
pub(super) fn CopyButton(text: String) -> Element {
    let (copy, copied) = use_copy_clipboard(None);
    let copy_text = text.clone();
    rsx! {
        Button {
            variant: ButtonVariant::Ghost,
            size: ButtonSize::Sm,
            aria_label: "Copy",
            onclick: move |_| copy(&copy_text),
            if copied() { "Copied" } else { "Copy" }
        }
    }
}
