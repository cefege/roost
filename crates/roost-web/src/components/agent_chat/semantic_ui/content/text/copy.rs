//! The Copy affordance shared by native code, diff and terminal output blocks.
//! The button writes the caller's complete source, not the previewed rows.

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::ui::{ButtonSize, IconButton, IconType};
use dioxus::prelude::*;

#[component]
pub(super) fn CopyButton(text: String) -> Element {
    let (copy, copied) = use_copy_clipboard(None, None);
    let copy_text = text.clone();
    rsx! {
        IconButton {
            icon: if copied() { IconType::Check } else { IconType::Copy },
            size: ButtonSize::IconXs,
            label: if copied() { "Copied" } else { "Copy" },
            title: if copied() { "Copied" } else { "Copy" },
            onclick: move |_| copy(&copy_text),
        }
    }
}
