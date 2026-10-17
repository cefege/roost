//! The phone's full-screen terminal grid: a toolbar (back, new, count, ⋮)
//! over a two-column grid of terminal cards; selection mode toggles cards and
//! closes the selected ones. Opened from `MobileDeckBar`'s count square and
//! rendered by `TerminalDeck` outside the deck element, so the deck's
//! transform does not confine it. Ports `WorkspaceTabsSheet` from
//! `apps/web/src/components/deck/MobileDeckBar.tsx`.

use crate::components::ui::{Icon, IconButton, IconType, StatusDot};
use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;

use super::workspace_tabs_menu::WorkspaceTabsMenu;
use crate::components::terminal::terminal_card::TerminalCard;
use crate::pump::use_store;

/// The toolbar's count text.
pub fn sheet_heading(selection_mode: bool, selected: usize, total: usize) -> String {
    if selection_mode {
        format!("{selected} selected")
    } else {
        format!("{total} tab{}", if total == 1 { "" } else { "s" })
    }
}

/// The delay before a card's entrance animation starts: 50ms per card, capped
/// at the seventh so a long grid does not hold its tail back for seconds.
pub fn card_enter_delay_ms(index: usize) -> u64 {
    (index.min(6) as u64) * 50
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
    let (sessions, agents): (Vec<_>, Vec<_>) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        (
            tab_ids
                .iter()
                .filter_map(|id| session_by_id(store, id).cloned())
                .collect(),
            tab_ids
                .iter()
                .filter_map(|tab_id| {
                    let conversation_id = tab_id.strip_prefix("agent:")?;
                    store.agent_chat.conversations.get(conversation_id).cloned()
                })
                .collect(),
        )
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
    let session_count = sessions.len();
    rsx! {
        div {
            class: "workspace-tabs-sheet fixed inset-0 z-[60] flex flex-col bg-background pt-[env(safe-area-inset-top,0px)]",
            "data-testid": "workspace-tabs-sheet",
            div {
                class: "workspace-tabs-head flex items-center gap-2 h-14 shrink-0 px-2 border-b border-border text-foreground",
                if !selection_mode() {
                    IconButton {
                        icon: IconType::ArrowLeft,
                        label: "Close terminal grid",
                        "data-testid": "workspace-tabs-back",
                        onclick: move |_| on_close_sheet.call(()),
                    }
                    button {
                        r#type: "button",
                        class: "flex size-9 shrink-0 cursor-pointer items-center justify-center gap-1 rounded-[10px] border-0 bg-transparent text-[var(--text-hi)] hover:bg-foreground/8 [-webkit-tap-highlight-color:transparent]",
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
                        icon: IconType::X,
                        label: "Exit selection",
                        "data-testid": "workspace-tabs-selection-exit",
                        onclick: move |_| exit_selection(),
                    }
                }
                span { class: "flex-1 basis-0 text-base font-medium", "{heading}" }
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
            if sessions.is_empty() && agents.is_empty() {
                div { class: "flex flex-col items-center gap-3 px-6 pt-16 pb-12 text-center text-muted-foreground", "data-testid": "workspace-tabs-empty",
                    div { class: "text-base font-medium text-foreground", "No tabs" }
                    div { class: "max-w-[42ch] text-sm", "Open one with the + above." }
                }
            } else {
                div { class: "grid grid-cols-2 content-start gap-2 flex-1 overflow-y-auto p-4",
                    for (index, session) in sessions.into_iter().enumerate() {
                        div { key: "{session.id.as_str()}", class: "min-w-0 animate-tc-card-enter motion-reduce:animate-none [animation-fill-mode:backwards]", style: format!("animation-delay: {}ms;", card_enter_delay_ms(index)), "data-flip-key": session.id.as_str().to_owned(),
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
                    for (index, conversation) in agents.into_iter().enumerate() {
                        {
                            let tab_id = roost_client_core::deck::agent_tab_id(&conversation.id);
                            let status = match conversation.run_state {
                                roost_protocol::wire::agent_chat::AgentRunState::Running => "info",
                                roost_protocol::wire::agent_chat::AgentRunState::Failed => "warn",
                                roost_protocol::wire::agent_chat::AgentRunState::Idle => "idle",
                            };
                            rsx! {
                                button {
                                    key: "{tab_id}",
                                    class: "min-w-0 animate-tc-card-enter motion-reduce:animate-none [animation-fill-mode:backwards]",
                                    style: format!("animation-delay: {}ms;", card_enter_delay_ms(session_count + index)),
                                    "data-testid": "workspace-agent-card-{conversation.id}",
                                    aria_current: (tab_id == selected_tab).then_some("page"),
                                    onclick: move |_| on_select.call(tab_id.clone()),
                                    Icon { icon: IconType::Bot, class: "size-4 shrink-0" }
                                    span { "{conversation.title}" }
                                    StatusDot { status: status.to_owned() }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
