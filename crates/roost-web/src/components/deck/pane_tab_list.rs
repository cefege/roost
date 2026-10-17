//! The filterable overflow menu for tabs that do not fit a pane strip: a
//! filter field, the matching tabs, and the selected tab's check. It is the
//! content of `PaneStrip`'s `DropdownMenu`, which owns placement, dismissal
//! and focus restore; this owns only its filter and highlight. Ports
//! `apps/web/src/components/deck/PaneTabList.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;

use super::deck_dom;
use crate::components::context_menu::MenuFocusEdge;
use crate::components::ui::{DropdownMenuItem, Icon, IconType};
use crate::pump::use_store;
use crate::session_naming::session_title;

/// The `DropdownMenu` root's `menu_id`; arrow keys from the filter focus its items by this id.
pub const TAB_LIST_MENU_ID: &str = "tab-list-menu";

/// Tabs whose title contains `filter`, case-insensitively, in strip order.
pub fn matching_tabs(tabs: &[(String, String)], filter: &str) -> Vec<(String, String)> {
    let needle = filter.trim().to_lowercase();
    tabs.iter()
        .filter(|(_, title)| title.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// The overflow list body. Mounted only while its menu is open, so the filter
/// and highlight start fresh on every open.
#[component]
pub fn PaneTabList(
    tab_ids: Vec<String>,
    selected_tab: String,
    on_select: EventHandler<String>,
    on_reveal_selected: EventHandler<()>,
    on_close: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let mut filter = use_signal(String::new);
    let mut highlighted = use_signal(|| 0_usize);
    let titled: Vec<(String, String)> = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        tab_ids
            .iter()
            .filter_map(|id| {
                session_by_id(store, id)
                    .map(|session| (id.clone(), session_title(store, session)))
                    .or_else(|| {
                        id.strip_prefix("agent:")
                            .and_then(|conversation_id| {
                                store.agent_chat.conversations.get(conversation_id)
                            })
                            .map(|conversation| (id.clone(), conversation.title.clone()))
                    })
            })
            .collect()
    };
    let matches = matching_tabs(&titled, &filter.read());
    let choose = move |session_id: String| {
        on_select.call(session_id.clone());
        on_close.call(());
        deck_dom::focus_tab_select(&session_id);
        // Re-picking the selected tab changes nothing the strip watches, so
        // the reveal is asked for here or the focus lands out of view.
        on_reveal_selected.call(());
    };
    let key_matches = matches.clone();
    rsx! {
        input {
            class: "w-full min-w-0 px-2 py-1 mb-1 bg-transparent border-0 border-b border-solid border-[var(--workbench-tab-border)] text-foreground outline-none",
            r#type: "text",
            value: "{filter}",
            "aria-label": "Filter terminals in this pane",
            placeholder: "Filter terminals…",
            "data-testid": "tab-list-filter",
            onmounted: move |event: MountedEvent| async move {
                let _ = event.data().set_focus(true).await;
            },
            oninput: move |event: FormEvent| {
                filter.set(event.value());
                highlighted.set(0);
            },
            onkeydown: move |event: KeyboardEvent| {
                // Escape and Tab bubble to the menu panel, which closes and restores focus.
                let key = event.key().to_string();
                match key.as_str() {
                    "ArrowDown" | "ArrowUp" => {
                        event.prevent_default();
                        event.stop_propagation();
                        if key_matches.is_empty() {
                            return;
                        }
                        let down = key == "ArrowDown";
                        highlighted.set(if down { 0 } else { key_matches.len() - 1 });
                        deck_dom::focus_menu(
                            TAB_LIST_MENU_ID,
                            if down {
                                MenuFocusEdge::First
                            } else {
                                MenuFocusEdge::Last
                            },
                        );
                    }
                    "Enter" => {
                        event.prevent_default();
                        event.stop_propagation();
                        let index = (*highlighted.peek()).min(key_matches.len().saturating_sub(1));
                        if let Some((session_id, _)) = key_matches.get(index) {
                            choose(session_id.clone());
                        }
                    }
                    _ => {}
                }
            },
        }
        if matches.is_empty() {
            div { class: "px-2 py-3 text-muted-foreground", "No matches" }
        }
        for (index, (session_id, title)) in matches.into_iter().enumerate() {
            DropdownMenuItem {
                key: "{session_id}",
                class: if session_id == selected_tab {
                    "min-h-[var(--workbench-tab-action-size)] bg-accent text-accent-foreground"
                } else if *highlighted.read() == index {
                    "min-h-[var(--workbench-tab-action-size)] bg-foreground/8"
                } else {
                    "min-h-[var(--workbench-tab-action-size)]"
                },
                test_id: "tab-list-item-{session_id}",
                onmouseenter: move |_| highlighted.set(index),
                onclick: {
                    let session_id = session_id.clone();
                    move |_| choose(session_id.clone())
                },
                Icon { icon: if session_id.starts_with("agent:") { IconType::Bot } else { IconType::Terminal }, class: "size-4 shrink-0 text-muted-foreground" }
                span { class: "flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap", "{title}" }
                if session_id == selected_tab {
                    Icon { icon: IconType::Check, class: "size-4 shrink-0" }
                }
            }
        }
    }
}
