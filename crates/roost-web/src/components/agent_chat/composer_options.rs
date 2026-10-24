//! Slash-command rows and argument hints for the agent composer's combobox.
//! The composer owns acceptance and draft state; Libero owns option navigation.
//! Command and argument labels retain their protocol-defined wording.

use dioxus::prelude::*;
use libero::components::{ComboboxOption, ComboboxOptionArgs, Text};

use super::slash_menu::SlashOption;

pub(super) fn render_slash_option(
    row: ComboboxOptionArgs<SlashOption>,
    accept: EventHandler<usize>,
) -> Element {
    let option = row.value;
    let index = row.index;
    rsx! {
        ComboboxOption {
            onpick: move |_| accept.call(index),
            span { "data-slot": "label", "{option.label}" }
            Text { size: "sm", color: "muted",
                if !option.usage.is_empty() { "{option.usage} " }
                "{option.summary}"
            }
        }
    }
}

pub(super) fn render_slash_usage(name: &str, args: &str, summary: &str) -> Element {
    rsx! {
        Text { size: "sm", color: "muted",
            span { "{name}" }
            if !args.is_empty() { " {args}" }
            " {summary}"
        }
    }
}
