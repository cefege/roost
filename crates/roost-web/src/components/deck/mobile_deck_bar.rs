//! The phone workspace bar, one 48px row: menu, the painted terminal's title,
//! its carrier chip, + (a sibling in the focused pane) and the count square
//! that opens the terminal grid. Rendered by `TerminalDeck` on a compact
//! host, twice while a swipe slides the neighbour's bar in. Ports
//! `apps/web/src/components/deck/MobileDeckBar.tsx` (the grid sheet it opens
//! is `workspace_tabs_sheet`).

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use libero::components::{Box, Pictogram, Text};
use roost_client_core::ClientEvent;
use roost_client_core::deck::deck_tab_badge;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

use super::pane_tab::direct_transport_label;
use super::workspace_tabs_sheet::WorkspaceTabsSheet;
use crate::components::agents::agent_status_indicator::agent_dot_color;
use crate::components::terminal::terminal_transport_indicator::TerminalTransportIndicator;
use crate::pump::use_store;
use crate::session_naming::session_title;
use libero::components::{ActionIcon, Button, Indicator};
use libero::sx::sx;
use roost_client_core::client::agents::status_policy::AgentDotStatus;

/// The bar's title for the painted terminal, and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BarReading {
    title: String,
    native_title: String,
    active: bool,
    agent_status: Option<AgentDotStatus>,
}

/// One bar.
#[component]
pub fn MobileDeckBar(
    tab_ids: Vec<String>,
    selected_tab: String,
    on_select: EventHandler<String>,
    on_close: EventHandler<String>,
    on_new_tab: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let mut sheet_open = use_signal(|| false);
    let sheet_generation = use_hook(|| {
        Rc::new(RefCell::new(
            crate::components::overlay_generation::OverlayGeneration::default(),
        ))
    });
    let reading = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let active_id = tab_ids.iter().find(|id| **id == selected_tab);
        let active = active_id.and_then(|id| session_by_id(store, id));
        let agent = active_id
            .and_then(|id| id.strip_prefix("agent:"))
            .and_then(|id| store.agent_chat.conversations.get(id));
        let title = active.map_or_else(
            || {
                agent.map_or_else(
                    || "Terminal".to_owned(),
                    |conversation| {
                        crate::components::agent_chat::display_conversation_title(
                            &conversation.title,
                        )
                        .to_owned()
                    },
                )
            },
            |session| session_title(store, session),
        );
        let carrier = active.and_then(|_| {
            direct_transport_label(session_terminal_transport_kind(store, &selected_tab))
        });
        BarReading {
            native_title: carrier
                .map_or_else(|| title.clone(), |carrier| format!("{title} — {carrier}")),
            title,
            active: active.is_some(),
            agent_status: agent.map(|conversation| match conversation.run_state {
                roost_protocol::wire::agent_chat::AgentRunState::Running => AgentDotStatus::Info,
                roost_protocol::wire::agent_chat::AgentRunState::Failed => AgentDotStatus::Warn,
                roost_protocol::wire::agent_chat::AgentRunState::Idle => AgentDotStatus::Idle,
            }),
        }
    };
    let badge = deck_tab_badge(
        tab_ids.len(),
        tab_ids.iter().position(|id| *id == selected_tab),
    );
    let open_drawer = pump.clone();
    let count_size = if badge.fraction { "xs" } else { "sm" };
    let bar_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .gap("sm")
            .height("3rem")
            .flex_shrink("0")
            .padding_inline("sm")
            .background("surface")
            .with("border-bottom", "1px solid var(--border)")
            .color("ink")
            .touch_action("pan-y")
    });
    let title_box_sx = use_hook(|| {
        sx().flex("1 1 0%")
            .min_width("0")
            .position("relative")
            .overflow("hidden")
            .height("3rem")
    });
    let title_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .gap("sm")
            .width("100%")
            .min_width("0")
            .height("100%")
            .font_size("md")
            .font_weight("500")
            .line_height("3rem")
    });
    let text_sx = use_hook(|| {
        sx().min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
    });
    rsx! {
        Box {
            component: "div",
            sx: bar_sx,
            "data-testid": "mobile-deck-bar",
            ActionIcon {
                icon: pictogram_icons_lucide::menu::outlined,
                aria_label: "Open sidebar",
                "data-testid": "mobile-deck-bar-menu",
                onclick: move |_| open_drawer.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer)),
            }
            Box { component: "div", sx: title_box_sx,
                Text {
                    component: "span",
                    title: "{reading.native_title}",
                    sx: title_sx,
                    if reading.agent_status.is_some() {
                        Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "1rem", height: "1rem" }
                    }
                    Text { component: "span", sx: text_sx, "{reading.title}" }
                    if let Some(status) = reading.agent_status.filter(|status| *status != AgentDotStatus::Idle) {
                        Indicator { color: agent_dot_color(status) }
                    }
                }
            }
            if reading.active {
                TerminalTransportIndicator { session_id: selected_tab.clone() }
            }
            ActionIcon {
                icon: pictogram_icons_lucide::plus::outlined,
                aria_label: "New terminal — same folder & server",
                title: "New terminal in this folder",
                "data-testid": "tab-new",
                onclick: move |_| on_new_tab.call(()),
            }
            Button {
                variant: "outlined",
                size: count_size,
                "data-testid": "mobile-tab-count",
                "aria-label": "Open terminal grid — {badge.description}",
                title: "{badge.description}",
                onclick: move |_| sheet_open.set(true),
                "{badge.text}"
            }
        }
        if sheet_open() {
            WorkspaceTabsSheet {
                key: "{tab_ids:?}-{selected_tab}",
                tab_ids: tab_ids.clone(),
                selected_tab: selected_tab.clone(),
                on_select: move |session_id| on_select.call(session_id),
                on_close: move |session_id| on_close.call(session_id),
                on_new_tab: move |()| on_new_tab.call(()),
                on_close_sheet: move |()| sheet_open.set(false),
                generation: sheet_generation.clone(),
            }
        }
    }
}
