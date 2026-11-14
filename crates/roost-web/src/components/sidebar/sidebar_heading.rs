//! The sidebar's sessions heading.

use dioxus::prelude::*;

/// The sidebar heading.
#[component]
pub fn SidebarHeading() -> Element {
    rsx! {
        div { class: "flex flex-col gap-2 px-3 pt-3", "data-testid": "sidebar-heading",
            span { class: "text-sm font-semibold", "Sessions" }
        }
    }
}
