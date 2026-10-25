//! The controlled sidebar filter input: every keystroke goes to the caller, Esc
//! clears a non-empty query, and a clear button or the shortcut hint sits at
//! its end. `SidebarRoot` owns the query and its debounce.

use dioxus::prelude::*;

use crate::components::ui::{
    ButtonSize, Icon, IconButton, IconType, Kbd, SidenavGroupContent, SidenavHeader, SidenavInput,
};

/// The id `SidebarRoot` focuses on Cmd-F.
pub const SIDEBAR_SEARCH_INPUT_ID: &str = "sidebar-search-input";

/// The filter input.
#[component]
pub fn SidebarSearch(
    query: String,
    on_change: EventHandler<String>,
    placeholder: Option<String>,
    /// The shortcut hint shown while the query is empty.
    shortcut_label: String,
) -> Element {
    let has_query = !query.is_empty();
    rsx! {
        SidenavHeader {
            "data-testid": "sidebar-search-wrapper",
            SidenavGroupContent { class: "relative",
                SidenavInput {
                    class: "pl-8 pr-10",
                    id: SIDEBAR_SEARCH_INPUT_ID,
                    value: query,
                    oninput: move |event: FormEvent| on_change.call(event.value()),
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Named(NamedKey::Escape) && has_query {
                            event.prevent_default();
                            event.stop_propagation();
                            on_change.call(String::new());
                        }
                    },
                    placeholder: placeholder.unwrap_or_else(|| "Filter folders…".to_owned()),
                    "aria-label": "Filter sidebar",
                    "data-testid": "sidebar-search",
                }
                Icon {
                    icon: IconType::Search,
                    class: "pointer-events-none absolute left-2 top-1/2 size-4 -translate-y-1/2 opacity-50",
                }
                span { class: "absolute right-1 top-1/2 -translate-y-1/2",
                    if has_query {
                        IconButton {
                            icon: IconType::X,
                            label: "Clear search",
                            size: ButtonSize::IconXs,
                            onclick: move |_| on_change.call(String::new()),
                            title: "Clear (Esc)",
                            "data-testid": "sidebar-search-clear",
                        }
                    } else {
                        span { "aria-hidden": "true", Kbd { {shortcut_label} } }
                    }
                }
            }
        }
    }
}
