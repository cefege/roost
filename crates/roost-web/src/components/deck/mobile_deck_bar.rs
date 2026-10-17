//! The phone workspace bar, one 48px row: menu, the painted terminal's title,
//! its carrier chip, + (a sibling in the focused pane) and the count square
//! that opens the terminal grid. Rendered by `TerminalDeck` on a compact
//! host, twice while a swipe slides the neighbour's bar in. Ports
//! `apps/web/src/components/deck/MobileDeckBar.tsx` (the grid sheet it opens
//! is `workspace_tabs_sheet`).

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::deck_tab_badge;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

use super::pane_tab::direct_transport_label;
use super::workspace_tabs_sheet::WorkspaceTabsSheet;
use crate::components::terminal::terminal_transport_indicator::TerminalTransportIndicator;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconType, StatusDot,
};
use crate::pump::use_store;
use crate::session_naming::session_title;

/// The bar's title for the painted terminal, and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BarReading {
    title: String,
    native_title: String,
    active: bool,
    agent_status: Option<&'static str>,
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
                    |conversation| conversation.title.clone(),
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
                roost_protocol::wire::agent_chat::AgentRunState::Running => "info",
                roost_protocol::wire::agent_chat::AgentRunState::Failed => "warn",
                roost_protocol::wire::agent_chat::AgentRunState::Idle => "idle",
            }),
        }
    };
    let badge = deck_tab_badge(
        tab_ids.len(),
        tab_ids.iter().position(|id| *id == selected_tab),
    );
    let open_drawer = pump.clone();
    let count_size = if badge.fraction {
        ButtonSize::Xs
    } else {
        ButtonSize::IconXs
    };
    let count_class = if badge.fraction {
        "h-7 w-auto min-w-7 px-1 text-xs font-semibold tabular-nums text-foreground"
    } else {
        "size-7 p-0 text-sm font-semibold text-foreground"
    };
    rsx! {
        div {
            class: "flex items-center gap-2 h-12 shrink-0 px-2 bg-card border-b border-border text-foreground touch-pan-y",
            "data-testid": "mobile-deck-bar",
            IconButton {
                icon: IconType::Menu,
                label: "Open sidebar",
                "data-testid": "mobile-deck-bar-menu",
                class: "shrink-0",
                onclick: move |_| open_drawer.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer)),
            }
            div { class: "flex-1 basis-0 min-w-0 relative overflow-hidden h-12",
                span {
                    title: "{reading.native_title}",
                    class: "flex items-center gap-2 w-full min-w-0 text-base font-medium leading-12",
                    if reading.agent_status.is_some() {
                        Icon { icon: IconType::Bot, class: "size-4 shrink-0" }
                    }
                    span { class: "shrink min-w-0 truncate", "{reading.title}" }
                    if let Some(status) = reading.agent_status.filter(|status| *status != "idle") {
                        StatusDot { status: status.to_owned() }
                    }
                }
            }
            if reading.active {
                TerminalTransportIndicator { session_id: selected_tab.clone() }
            }
            IconButton {
                icon: IconType::Plus,
                label: "New terminal — same folder & server",
                title: "New terminal in this folder",
                "data-testid": "tab-new",
                onclick: move |_| on_new_tab.call(()),
            }
            Button {
                variant: ButtonVariant::Outline,
                size: count_size,
                class: Some(count_class.to_owned()),
                "data-testid": "mobile-tab-count",
                "aria-label": "Open terminal grid — {badge.description}",
                title: "{badge.description}",
                onclick: move |_| sheet_open.set(true),
                span { "{badge.text}" }
            }
        }
        if sheet_open() {
            WorkspaceTabsSheet {
                tab_ids: tab_ids.clone(),
                selected_tab: selected_tab.clone(),
                on_select: move |session_id| on_select.call(session_id),
                on_close: move |session_id| on_close.call(session_id),
                on_new_tab: move |()| on_new_tab.call(()),
                on_close_sheet: move |()| sheet_open.set(false),
            }
        }
    }
}
