//! The phone's full-screen terminal grid: a toolbar (back, new, count, ⋮) over
//! a two-column grid of terminal and agent cards. Selection mode toggles cards
//! and closes the selected ones. `MobileDeckBar` mounts it as a top Libero
//! drawer only while open, so dismissal goes through its `on_close_sheet`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::components::overlay_generation::OverlayGeneration;
use dioxus::prelude::*;
use libero::components::{ActionIcon, Box, Button, Indicator, Paper, Pictogram, Text};
use libero::context::Dismiss;
use libero::hooks::{DrawerOptions, ModalScope, use_drawer};
use libero::sx::sx;

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
    generation: Rc<RefCell<OverlayGeneration>>,
) -> Element {
    let root_sx = use_hook(|| {
        sx().height("100%")
            .bottom("0")
            .display("flex")
            .flex_direction("column")
            .overflow("hidden")
            .border("0")
            .background("surface")
            .padding("0")
            .with("padding-top", "env(safe-area-inset-top, 0px)")
            .box_shadow("none")
    });
    let toolbar_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .gap("sm")
            .height("3.5rem")
            .flex_shrink("0")
            .padding_inline("sm")
            .with("border-bottom", "1px solid var(--border)")
            .color("ink")
    });
    let heading_sx = use_hook(|| sx().flex("1 1 0%").font_size("md").font_weight("500"));
    let empty_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("center")
            .padding_inline("lg")
            .padding_top("4rem")
            .padding_bottom("3rem")
    });
    let grid_sx = use_hook(|| {
        sx().display("grid")
            .with("grid-template-columns", "repeat(2, minmax(0, 1fr))")
            .align_content("start")
            .gap("sm")
            .flex("1")
            .overflow_y("auto")
            .padding("md")
    });
    let entering_sx = use_hook(|| {
        sx().min_width("0")
            .with(
                "animation",
                "tc-card-enter 200ms cubic-bezier(0.05, 0.7, 0.1, 1) backwards",
            )
            .media("(prefers-reduced-motion: reduce)", sx().animation("none"))
    });
    let agent_card_sx = use_hook(|| sx().gap("0").overflow("hidden"));
    let agent_button_sx = use_hook(|| {
        sx().height("auto")
            .width("100%")
            .min_width("0")
            .display("flex")
            .flex_direction("row")
            .align_items("center")
            .justify_content("flex-start")
            .gap("sm")
            .border_radius("inherit")
            .padding_inline("md")
            .padding_block("md")
            .text_align("left")
            .font_weight("normal")
    });
    let agent_title_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
    });
    let pump = use_store();
    let pane_registry = crate::components::terminal::pane_registry::use_pane_registry();
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
    let close_all = Callback::new({
        let snapshot = tab_ids.clone();
        move |()| {
            for id in &snapshot {
                on_close.call(id.clone());
            }
            on_close_sheet.call(());
        }
    });
    let close_selected = Callback::new({
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
    });
    let select_all = Callback::new({
        let tab_ids = tab_ids.clone();
        move |()| selected_ids.set(tab_ids.clone())
    });
    let heading = sheet_heading(selection_mode(), selected_ids.read().len(), tab_ids.len());
    let session_count = sessions.len();
    let opening_generation = use_hook(|| generation.borrow_mut().begin());
    let mut did_open = use_signal(|| false);
    let dismiss = Callback::new(move |reason: Dismiss| {
        if matches!(reason, Dismiss::Escape | Dismiss::Back) && selection_mode() {
            exit_selection();
            false
        } else {
            matches!(reason, Dismiss::Escape | Dismiss::Back)
        }
    });
    let drawer = use_drawer(
        DrawerOptions {
            anchor: "top".into(),
            size: "100dvh".into(),
            aria_label: Some("Workspace tabs".to_owned()),
            close_button: false,
            ondismiss: Some(dismiss),
            ..Default::default()
        },
        move |_: ModalScope<()>| {
            rsx! {
                Box {
                    component: "div",
                    sx: root_sx.clone(),
                    "data-testid": "workspace-tabs-sheet",
                    Box { component: "div", sx: toolbar_sx.clone(),
                        if !selection_mode() {
                            ActionIcon {
                                icon: pictogram_icons_lucide::arrow_left::outlined,
                                aria_label: "Close terminal grid",
                                "data-testid": "workspace-tabs-back",
                                onclick: move |_| on_close_sheet.call(()),
                            }
                            ActionIcon {
                                icon: pictogram_icons_lucide::plus::outlined,
                                aria_label: "New terminal",
                                title: "New terminal in this folder",
                                "data-testid": "workspace-tabs-new",
                                onclick: move |_| on_new_tab.call(()),
                            }
                        } else {
                            ActionIcon {
                                icon: pictogram_icons_lucide::x::outlined,
                                aria_label: "Exit selection",
                                "data-testid": "workspace-tabs-selection-exit",
                                onclick: move |_| exit_selection(),
                            }
                        }
                        Text { component: "span", sx: heading_sx.clone(), "{heading}" }
                        WorkspaceTabsMenu {
                            selection_mode: selection_mode(),
                            on_close_all: move |()| close_all.call(()),
                            on_select_tabs: move |()| {
                                selected_ids.set(Vec::new());
                                selection_mode.set(true);
                            },
                            on_select_all: move |()| select_all.call(()),
                            on_close_selected: move |()| close_selected.call(()),
                        }
                    }
                    if sessions.is_empty() && agents.is_empty() {
                        Box {
                            component: "div",
                            sx: empty_sx.clone(),
                            "data-testid": "workspace-tabs-empty",
                            Text { "No tabs. Open one with the + above." }
                        }
                    } else {
                        Box { component: "div", sx: grid_sx.clone(),
                            for (index, session) in sessions.iter().cloned().enumerate() {
                                Box { component: "div", key: "{session.id.as_str()}", sx: entering_sx.clone(), style: format!("animation-delay: {}ms;", card_enter_delay_ms(index)), "data-flip-key": session.id.as_str().to_owned(),
                                    TerminalCard {
                                        active: session.id.as_str() == selected_tab,
                                        selected: selected_ids.read().iter().any(|id| id == session.id.as_str()),
                                        session: session.clone(),
                                        pump: pump.clone(),
                                        pane_registry: pane_registry.clone(),
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
                            for (index, conversation) in agents.iter().cloned().enumerate() {
                                {
                                    let tab_id = roost_client_core::deck::agent_tab_id(&conversation.id);
                                    let status = match conversation.run_state {
                                        roost_protocol::wire::agent_chat::AgentRunState::Running => roost_client_core::client::agents::status_policy::AgentDotStatus::Info,
                                        roost_protocol::wire::agent_chat::AgentRunState::Failed => roost_client_core::client::agents::status_policy::AgentDotStatus::Warn,
                                        roost_protocol::wire::agent_chat::AgentRunState::Idle => roost_client_core::client::agents::status_policy::AgentDotStatus::Idle,
                                    };
                                    rsx! {
                                        Box {
                                            component: "div",
                                            key: "{tab_id}",
                                            sx: entering_sx.clone(),
                                            style: format!("animation-delay: {}ms;", card_enter_delay_ms(session_count + index)),
                                            Paper {
                                                sx: agent_card_sx.clone(),
                                                Button {
                                                    variant: "standard",
                                                    sx: agent_button_sx.clone(),
                                                    "data-testid": "workspace-agent-card-{conversation.id}",
                                                    "aria-current": (tab_id == selected_tab).then_some("page"),
                                                    onclick: move |_| on_select.call(tab_id.clone()),
                                                    Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "1rem", height: "1rem" }
                                                    Text { component: "span", sx: agent_title_sx.clone(), "{crate::components::agent_chat::display_conversation_title(&conversation.title)}" }
                                                    Indicator { color: crate::components::agents::agent_status_indicator::agent_dot_color(status) }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    );
    use_effect(move || {
        if did_open() {
            return;
        }
        did_open.set(true);
        let current_generation = generation.clone();
        drawer.open().onresult(move |_| {
            if current_generation.borrow_mut().finish(opening_generation) {
                on_close_sheet.call(());
            }
        });
    });
    rsx! {}
}
