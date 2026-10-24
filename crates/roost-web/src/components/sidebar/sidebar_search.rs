//! The controlled sidebar filter input: every keystroke goes to the caller, Esc
//! clears a non-empty query, and a clear button or the shortcut hint sits at
//! its end. `SidebarRoot` owns the query and its debounce.

use dioxus::prelude::*;
use libero::components::{ActionIcon, Box, Kbd, Pictogram, TextField};
use libero::sx::sx;

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
    let wrapper_sx = use_hook(|| {
        sx().border_bottom("1px solid")
            .border_color("muted.4")
            .padding_inline("md")
            .padding_block("sm")
    });
    let field_sx = use_hook(|| sx().width("100%"));
    let trailing = if has_query {
        Some(rsx! {
            ActionIcon {
                aria_label: "Clear search",
                onclick: move |_| on_change.call(String::new()),
                title: "Clear (Esc)",
                "data-testid": "sidebar-search-clear",
                Pictogram { icon: pictogram_icons_lucide::x::outlined, width: "1rem", height: "1rem" }
            }
        })
    } else {
        Some(rsx! { Kbd { "{shortcut_label}" } })
    };
    rsx! {
        Box { component: "div", sx: wrapper_sx,
            "data-testid": "sidebar-search-wrapper",
            TextField {
                sx: field_sx,
                id: SIDEBAR_SEARCH_INPUT_ID,
                value: query,
                oninput: move |value: String| on_change.call(value),
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Named(NamedKey::Escape) && has_query {
                        event.prevent_default();
                        event.stop_propagation();
                        on_change.call(String::new());
                    }
                },
                placeholder: placeholder.unwrap_or_else(|| "Search sessions, workspaces…".to_owned()),
                aria_label: "Filter sidebar",
                leading: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::search::outlined, width: "1rem", height: "1rem" } }),
                trailing,
                "data-testid": "sidebar-search",
            }
        }
    }
}
