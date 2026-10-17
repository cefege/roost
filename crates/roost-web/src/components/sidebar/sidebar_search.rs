//! The controlled sidebar filter input: every keystroke goes to the caller, Esc
//! clears a non-empty query, and a clear button or the shortcut hint sits at
//! its end. `SidebarRoot` owns the query and its debounce.

use dioxus::prelude::*;

use crate::components::ui::input_group::{
    InputGroup, InputGroupAddon, InputGroupAddonAlign, InputGroupInput,
};
use crate::components::ui::{ButtonSize, Icon, IconButton, IconType, Kbd};

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
            class: "border-b border-border px-3 py-2",
            "data-testid": "sidebar-search-wrapper",
            InputGroup { class: "h-8 rounded-md border-0 bg-transparent shadow-none dark:bg-transparent",
                InputGroupAddon { class: "text-muted-foreground",
                    Icon { icon: IconType::Search, class: "size-4 shrink-0" }
                }
                InputGroupInput {
                    id: SIDEBAR_SEARCH_INPUT_ID,
                    class: "text-sm text-foreground",
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
                    aria_label: "Filter sidebar",
                    test_id: "sidebar-search",
                }
                if has_query {
                    InputGroupAddon { align: InputGroupAddonAlign::InlineEnd,
                        IconButton {
                            icon: IconType::X,
                            label: "Clear search",
                            size: ButtonSize::IconSm,
                            class: "shrink-0",
                            onclick: move |_| on_change.call(String::new()),
                            title: "Clear (Esc)",
                            "data-testid": "sidebar-search-clear",
                        }
                    }
                } else {
                    InputGroupAddon { align: InputGroupAddonAlign::InlineEnd,
                        span { "aria-hidden": "true",
                            Kbd { class: "shrink-0", {shortcut_label} }
                        }
                    }
                }
            }
        }
    }
}
