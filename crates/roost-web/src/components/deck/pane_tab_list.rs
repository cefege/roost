//! The filterable overflow menu for tabs that do not fit a pane strip: a
//! filter field, the matching tabs, and the selected tab's check. It owns only
//! its focus, filter and highlight; `PaneStrip` keeps the anchor and every
//! layout callback. Ports `apps/web/src/components/deck/PaneTabList.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;

use super::deck_dom::{self, DeckContainer};
use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, MenuFocusEdge, anchored_menu_surface_style,
    use_floating_menu_dismiss,
};
use crate::components::md::{Icon, IconSize};
use crate::pump::use_store;
use crate::session_naming::session_title;

const POPUP_ID: &str = "tab-list-popup";
const ITEMS_ID: &str = "tab-list-popup-items";

/// Tabs whose title contains `filter`, case-insensitively, in strip order.
pub fn matching_tabs(tabs: &[(String, String)], filter: &str) -> Vec<(String, String)> {
    let needle = filter.trim().to_lowercase();
    tabs.iter()
        .filter(|(_, title)| title.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// The overflow list.
#[component]
pub fn PaneTabList(
    position: AnchoredMenuPos,
    tab_ids: Vec<String>,
    selected_tab: String,
    trigger_id: String,
    on_select: EventHandler<String>,
    on_reveal_selected: EventHandler<()>,
    on_close: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let container = try_use_context::<DeckContainer>();
    let mut filter = use_signal(String::new);
    let mut highlighted = use_signal(|| 0_usize);
    let titled: Vec<(String, String)> = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        tab_ids
            .iter()
            .filter_map(|id| {
                session_by_id(store, id).map(|session| (id.clone(), session_title(store, session)))
            })
            .collect()
    };
    let matches = matching_tabs(&titled, &filter.read());
    let restore_trigger = {
        let trigger_id = trigger_id.clone();
        move || deck_dom::focus_by_id(&trigger_id)
    };
    use_floating_menu_dismiss(
        on_close,
        Some(EventHandler::new({
            let restore_trigger = restore_trigger.clone();
            move |()| {
                on_close.call(());
                restore_trigger();
            }
        })),
        vec![trigger_id.clone(), POPUP_ID.to_owned()],
    );
    let choose = move |session_id: String| {
        on_select.call(session_id.clone());
        on_close.call(());
        deck_dom::focus_tab_select(&session_id);
        // Re-picking the selected tab changes nothing the strip watches, so
        // the reveal is asked for here or the focus lands out of view.
        on_reveal_selected.call(());
    };
    let placed = match container {
        Some(container) => {
            deck_dom::menu_pos_in(position, container.origin(), deck_dom::viewport_width())
        }
        None => position,
    };
    let style =
        anchored_menu_surface_style(placed, "var(--workbench-tab-menu-min-width)", None, "");
    let key_matches = matches.clone();
    rsx! {
        div {
            id: POPUP_ID,
            "data-testid": "tab-list-popup",
            class: "df-menu-enter workbench-tab-list",
            "aria-label": "Open terminals in this pane",
            style,
            div { class: "workbench-tab-list__filter",
                Icon { name: "search", class: "workbench-tab-list__filter-icon", size: IconSize::Sm }
                input {
                    class: "workbench-tab-list__input",
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
                    onkeydown: {
                        let restore_trigger = restore_trigger.clone();
                        move |event: KeyboardEvent| {
                            let key = event.key().to_string();
                            match key.as_str() {
                                "Escape" | "Tab" => {
                                    let restore_trigger = restore_trigger.clone();
                                    deck_dom::run_menu_keys(&event, ITEMS_ID, move || {
                                        on_close.call(());
                                        restore_trigger();
                                    }, move || on_close.call(()));
                                }
                                "ArrowDown" | "ArrowUp" => {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    if key_matches.is_empty() {
                                        return;
                                    }
                                    let down = key == "ArrowDown";
                                    highlighted.set(if down { 0 } else { key_matches.len() - 1 });
                                    deck_dom::focus_menu(ITEMS_ID, if down { MenuFocusEdge::First } else { MenuFocusEdge::Last });
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
                        }
                    },
                }
            }
            div {
                id: ITEMS_ID,
                class: "workbench-tab-list__items",
                role: "menu",
                "aria-label": "Open terminals in this pane",
                onkeydown: {
                    let restore_trigger = restore_trigger.clone();
                    move |event: KeyboardEvent| {
                        let restore_trigger = restore_trigger.clone();
                        deck_dom::run_menu_keys(&event, ITEMS_ID, move || {
                            on_close.call(());
                            restore_trigger();
                        }, move || on_close.call(()));
                    }
                },
                if matches.is_empty() {
                    div { class: "workbench-tab-list__empty", "No matches" }
                }
                for (index, (session_id, title)) in matches.into_iter().enumerate() {
                    CtxMenuItem {
                        key: "{session_id}",
                        class: "workbench-tab-list__item",
                        testid: "tab-list-item-{session_id}",
                        selected: session_id == selected_tab,
                        highlighted: *highlighted.read() == index,
                        onmouseenter: move |_| highlighted.set(index),
                        onfocus: move |_| highlighted.set(index),
                        onclick: {
                            let session_id = session_id.clone();
                            move |_| choose(session_id.clone())
                        },
                        Icon { name: "terminal", class: "workbench-tab-list__item-icon", size: IconSize::Sm }
                        span { class: "workbench-tab-list__item-label", "{title}" }
                        if session_id == selected_tab {
                            Icon { name: "check", class: "workbench-tab-list__item-check", size: IconSize::Sm }
                        }
                    }
                }
            }
        }
    }
}
