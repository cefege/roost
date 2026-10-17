//! The controlled sidebar filter input: every keystroke goes to the caller, Esc
//! clears a non-empty query, and a clear button or the shortcut hint sits at
//! its end. Ports `apps/web/src/components/sidebar/SidebarSearch.tsx`;
//! `SidebarRoot` owns the query and its debounce.

use dioxus::prelude::*;

use crate::components::ui::{ButtonSize, Icon, IconButton, IconType};

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
        div {
            class: "flex items-center gap-2 border-b border-border px-3 py-2 text-muted-foreground",
            "data-testid": "sidebar-search-wrapper",
            Icon { icon: IconType::Search, class: "size-4 shrink-0" }
            input {
                id: SIDEBAR_SEARCH_INPUT_ID,
                class: "min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground",
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
                    icon: IconType::X,
                    label: "Clear search",
                    size: ButtonSize::IconSm,
                    class: "shrink-0",
                    onclick: move |_| on_change.call(String::new()),
                    title: "Clear (Esc)",
                    "data-testid": "sidebar-search-clear",
                }
            } else {
                span {
                    class: "shrink-0 rounded-sm border border-border px-1 font-mono text-xs text-muted-foreground",
                    "aria-hidden": "true",
                    {shortcut_label}
                }
            }
        }
    }
}
