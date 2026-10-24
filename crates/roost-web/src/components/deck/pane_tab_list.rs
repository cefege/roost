//! A filterable Libero combobox for tabs that do not fit a pane strip. The
//! active row and arrow-key navigation belong to Libero; this component keeps
//! the product's title filtering and selection behavior.

use dioxus::prelude::*;
use libero::components::Box;
use libero::components::{
    ActionIcon, Combobox, ComboboxOption, ComboboxOptionArgs, ComboboxState, Pictogram, TextField,
};
use libero::sx::sx;
use roost_client_core::store::selectors::session_by_id;

use super::deck_dom;
use crate::pump::use_store;
use crate::session_naming::session_title;

/// Tabs whose title contains `filter`, case-insensitively, in strip order.
pub fn matching_tabs(tabs: &[(String, String)], filter: &str) -> Vec<(String, String)> {
    let needle = filter.trim().to_lowercase();
    tabs.iter()
        .filter(|(_, title)| title.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// The overflow list and its strip trigger. Mounted only while tabs overflow.
#[component]
pub fn PaneTabList(
    tab_ids: Vec<String>,
    selected_tab: String,
    state: ComboboxState,
    on_select: EventHandler<String>,
    on_reveal_selected: EventHandler<()>,
    on_open: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let mut filter = use_signal(String::new);
    let filter_sx = use_hook(|| {
        sx().width("12rem")
            .min_width("0")
            .height("auto")
            .padding_inline("sm")
            .padding_block("xs")
            .margin_bottom("xs")
            .border("0")
            .border_bottom("1px solid var(--workbench-tab-border)")
            .border_radius("0")
            .color("ink")
            .outline("none")
    });
    let list_sx = use_hook(|| {
        sx().width("auto")
            .min_width("var(--workbench-tab-menu-min-width)")
            .max_width("24rem")
            .max_height("24rem")
            .overflow_y("auto")
    });
    let trigger_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .gap("xs")
    });
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
                            .map(|conversation| {
                                (
                                    id.clone(),
                                    crate::components::agent_chat::display_conversation_title(
                                        &conversation.title,
                                    )
                                    .to_owned(),
                                )
                            })
                    })
            })
            .collect()
    };
    let matches = matching_tabs(&titled, &filter());
    let choose = move |session_id: String| {
        state.close();
        on_select.call(session_id.clone());
        deck_dom::focus_tab_select(&session_id);
        on_reveal_selected.call(());
    };
    rsx! {
        Combobox {
            state,
            options: matches,
            empty_label: "No matches",
            sx: list_sx,
            option: move |row: ComboboxOptionArgs<(String, String)>| {
                let (session_id, title) = row.value;
                let selected = session_id == selected_tab;
                let test_id = format!("tab-list-item-{session_id}");
                let pick_id = session_id.clone();
                rsx! {
                    ComboboxOption {
                        key: "{session_id}",
                        selected,
                        onpick: move |_| choose(pick_id.clone()),
                        "data-testid": test_id,
                        Pictogram {
                            icon: if session_id.starts_with("agent:") {
                                pictogram_icons_lucide::bot::outlined
                            } else {
                                pictogram_icons_lucide::terminal::outlined
                            },
                            width: "1rem",
                            height: "1rem",
                        }
                        span { "data-slot": "label", "{title}" }
                        if selected {
                            Pictogram {
                                icon: pictogram_icons_lucide::check::outlined,
                                width: "1rem",
                                height: "1rem",
                            }
                        }
                    }
                }
            },
            Box {
                component: "div",
                sx: trigger_sx,
                ActionIcon {
                    icon: pictogram_icons_lucide::chevron_down::outlined,
                    aria_label: "All terminals in this pane",
                    title: "All terminals in this pane",
                    "data-testid": "tab-overflow",
                    attributes: if state.is_open() { Vec::new() } else { state.a11y_attributes() },
                    onclick: move |_| {
                        if state.is_open() {
                            state.close();
                        } else {
                            on_open.call(());
                            filter.set(String::new());
                            state.open();
                        }
                    },
                }
                if state.is_open() {
                    TextField {
                        label: "Filter terminals in this pane",
                        value: filter(),
                        placeholder: "Filter terminals…",
                        sx: filter_sx.clone(),
                        attributes: state.a11y_attributes(),
                        "data-testid": "tab-list-filter",
                        oninput: move |value: String| filter.set(value),
                        onmounted: move |event: MountedEvent| async move {
                            let _ = event.data().set_focus(true).await;
                        },
                    }
                }
            }
        }
    }
}
