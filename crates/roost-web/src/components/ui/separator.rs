//! `Separator`: the hairline divider between rows, menu groups and toolbar clusters.
//! Decorative structure with `role="separator"`; `ButtonGroupSeparator` composes it.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeparatorOrientation {
    #[default]
    Horizontal,
    Vertical,
}

#[component]
pub fn Separator(
    #[props(into, optional)] class: Option<String>,
    #[props(default = SeparatorOrientation::default())] orientation: SeparatorOrientation,
    /// Purely visual: `role="none"`, as Radix's `decorative`. Required inside a
    /// container whose role admits only specific children, such as a listbox.
    #[props(default = false)]
    decorative: bool,
) -> Element {
    let orientation_class = match orientation {
        SeparatorOrientation::Horizontal => "w-full h-[1px]",
        SeparatorOrientation::Vertical => "h-full w-[1px]",
    };
    let merged_class = tw_merge!(
        "shrink-0 bg-border",
        orientation_class,
        class.as_deref().unwrap_or("")
    );

    rsx! {
        div { class: "{merged_class}", role: if decorative { "none" } else { "separator" } }
    }
}
