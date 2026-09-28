//! The phone's full-screen terminal grid: a toolbar (back, new, count, ⋮)
//! over a two-column grid of terminal cards; selection mode toggles cards and
//! closes the selected ones. Opened from `MobileDeckBar`'s count square and
//! rendered by `TerminalDeck` outside the deck element, so the deck's
//! transform does not confine it. Ports `WorkspaceTabsSheet` from
//! `apps/web/src/components/deck/MobileDeckBar.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;

use super::workspace_tabs_menu::WorkspaceTabsMenu;
use crate::components::md::IconButton;
use crate::components::terminal::terminal_card::TerminalCard;
use crate::pump::use_store;

/// The toolbar's count text.
pub fn sheet_heading(selection_mode: bool, selected: usize, total: usize) -> String {
    if selection_mode {
        format!("{selected} selected")
    } else {
        format!("{total} terminal{}", if total == 1 { "" } else { "s" })
    }
}

/// The grid.
#[component]
pub fn WorkspaceTabsSheet(
    tab_ids: Vec<String>,
    selected_tab: String,
    on_select: EventHandler<String>,
    on_close: EventHandler<String>,
    on_new_tab: EventHandler<()>,
    on_close_sheet: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let mut selection_mode = use_signal(|| false);
    let mut selected_ids = use_signal(Vec::<String>::new);
    let sessions: Vec<_> = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        tab_ids
            .iter()
            .filter_map(|id| session_by_id(store, id).cloned())
            .collect()
    };
    let mut exit_selection = move || {
        selection_mode.set(false);
        selected_ids.set(Vec::new());
    };
    // Snapshotted before closing: each close commits and shrinks the list.
    let close_all = {
        let snapshot = tab_ids.clone();
        move |()| {
            for id in &snapshot {
                on_close.call(id.clone());
            }
            on_close_sheet.call(());
        }
    };
    let close_selected = {
        let tab_ids = tab_ids.clone();
        move |()| {
            let chosen = selected_ids.peek().clone();
            let snapshot: Vec<String> = tab_ids
                .iter()
                .filter(|id| chosen.contains(id))
                .cloned()
                .collect();
            exit_selection();
            for id in snapshot {
                on_close.call(id);
            }
        }
    };
    let select_all = {
        let tab_ids = tab_ids.clone();
        move |()| selected_ids.set(tab_ids.clone())
    };
    let heading = sheet_heading(selection_mode(), selected_ids.read().len(), tab_ids.len());
    rsx! {
        div {
            class: "workspace-tabs-sheet",
            "data-testid": "workspace-tabs-sheet",
            style: "position: fixed; inset: 0; z-index: 60; background: var(--md-surface-container-lowest); display: flex; flex-direction: column; padding-top: env(safe-area-inset-top, 0px);",
            div {
                class: "workspace-tabs-head",
                style: "display: flex; align-items: center; gap: var(--md-space-2); height: calc(var(--md-space-9) + var(--md-space-2)); flex-shrink: 0; padding: 0 var(--md-space-2); border-bottom: var(--workbench-border-width) solid var(--border-subtle); color: var(--text-hi);",
                if !selection_mode() {
                    IconButton {
                        icon: "arrow_back",
                        label: "Close terminal grid",
                        "data-testid": "workspace-tabs-back",
                        onclick: move |_| on_close_sheet.call(()),
                    }
                    button {
                        r#type: "button",
                        class: "mobile-deck-new",
                        "data-testid": "workspace-tabs-new",
                        "aria-label": "New terminal",
                        title: "New terminal in this folder",
                        onclick: move |_| on_new_tab.call(()),
                        svg { width: "18", height: "18", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2.5", stroke_linecap: "round",
                            path { d: "M12 5v14M5 12h14" }
                        }
                    }
                } else {
                    IconButton {
                        icon: "close",
                        label: "Exit selection",
                        "data-testid": "workspace-tabs-selection-exit",
                        onclick: move |_| exit_selection(),
                    }
                }
                span { style: "flex: 1 1 0; font-size: var(--md-title-m-size); font-weight: var(--md-title-s-weight);", "{heading}" }
                WorkspaceTabsMenu {
                    selection_mode: selection_mode(),
                    on_close_all: close_all,
                    on_select_tabs: move |()| {
                        selected_ids.set(Vec::new());
                        selection_mode.set(true);
                    },
                    on_select_all: select_all,
                    on_close_selected: close_selected,
                }
            }
            if sessions.is_empty() {
                div { class: "home-landing-empty", "data-testid": "workspace-tabs-empty", style: "padding-top: calc(var(--md-space-9) + var(--md-space-4));",
                    div { class: "home-landing-empty-title", "No terminals" }
                    div { class: "home-landing-empty-sub", "Open one with the + above." }
                }
            } else {
                div { class: "workspace-tabs-grid", style: "padding: var(--md-space-4); overflow-y: auto; flex: 1 1 0;",
                    for session in sessions {
                        div { key: "{session.id.as_str()}", class: "terminal-card-wrap", "data-flip-key": session.id.as_str().to_owned(),
                            TerminalCard {
                                active: session.id.as_str() == selected_tab,
                                selected: selected_ids.read().iter().any(|id| id == session.id.as_str()),
                                session: session.clone(),
                                on_select: move |id| on_select.call(id),
                                on_close: move |closed: roost_client_core::store::Session| on_close.call(closed.id.as_str().to_owned()),
                                on_close_sheet: move |()| on_close_sheet.call(()),
                                selection_mode: selection_mode(),
                                on_toggle_select: move |id: String| {
                                    let mut chosen = selected_ids.write();
                                    match chosen.iter().position(|selected| *selected == id) {
                                        Some(index) => {
                                            chosen.remove(index);
                                        }
                                        None => chosen.push(id),
                                    }
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}
