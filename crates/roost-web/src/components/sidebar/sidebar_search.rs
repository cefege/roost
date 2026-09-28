//! The controlled sidebar filter input: every keystroke goes to the caller, Esc
//! clears a non-empty query, and a clear button or the shortcut hint sits at
//! its end. Ports `apps/web/src/components/sidebar/SidebarSearch.tsx`;
//! `SidebarRoot` owns the query and its debounce.

use dioxus::prelude::*;

use crate::components::md::{Icon, IconButton, IconButtonSize, IconSize};

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
        div { class: "df-search workbench-sidebar-search", "data-testid": "sidebar-search-wrapper",
            Icon { name: "search", class: "workbench-sidebar-search__icon", size: IconSize::Sm }
            input {
                id: SIDEBAR_SEARCH_INPUT_ID,
                class: "workbench-sidebar-search__input",
                r#type: "text",
                value: query,
                oninput: move |event: FormEvent| on_change.call(event.value()),
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Escape && has_query {
                        event.prevent_default();
                        event.stop_propagation();
                        on_change.call(String::new());
                    }
                },
                placeholder: placeholder.unwrap_or_else(|| "Search sessions, workspaces…".to_owned()),
                "aria-label": "Filter sidebar",
                "data-testid": "sidebar-search",
            }
            if has_query {
                IconButton {
                    icon: "close",
                    label: "Clear search",
                    size: IconButtonSize::IconSm,
                    class: "workbench-sidebar-search__clear",
                    onclick: move |_| on_change.call(String::new()),
                    title: "Clear (Esc)",
                    "data-testid": "sidebar-search-clear",
                }
            } else {
                span {
                    class: "df-search-kbd workbench-sidebar-search__shortcut",
                    "aria-hidden": "true",
                    {shortcut_label}
                }
            }
        }
    }
}
