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
use crate::components::md::IconButton;
use crate::components::terminal::terminal_transport_indicator::TerminalTransportIndicator;
use crate::pump::use_store;
use crate::session_naming::session_title;

/// The bar's title for the painted terminal, and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BarReading {
    title: String,
    native_title: String,
    active: bool,
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
        let active = tab_ids
            .iter()
            .find(|id| **id == selected_tab)
            .and_then(|id| session_by_id(store, id));
        let title = active.map_or_else(
            || "Terminal".to_owned(),
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
        }
    };
    let badge = deck_tab_badge(
        tab_ids.len(),
        tab_ids.iter().position(|id| *id == selected_tab),
    );
    let open_drawer = pump.clone();
    rsx! {
        div {
            class: "mobile-deck-bar",
            "data-testid": "mobile-deck-bar",
            style: "display: flex; align-items: center; gap: var(--md-space-2); height: var(--md-space-9); flex-shrink: 0; padding: 0 var(--md-space-2); background: var(--surface-1); border-bottom: var(--workbench-border-width) solid var(--border-subtle); color: var(--text-hi); touch-action: pan-y;",
            IconButton {
                icon: "menu",
                label: "Open sidebar",
                "data-testid": "mobile-deck-bar-menu",
                style: "flex-shrink: 0;",
                onclick: move |_| open_drawer.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer)),
            }
            div { style: "flex: 1 1 0; min-width: 0; position: relative; overflow: hidden; height: var(--md-space-9);",
                span {
                    title: "{reading.native_title}",
                    style: "font-size: var(--md-title-s-size); font-weight: var(--md-title-m-weight); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; line-height: var(--md-space-9); display: block; width: 100%;",
                    "{reading.title}"
                }
            }
            if reading.active {
                TerminalTransportIndicator { session_id: selected_tab.clone() }
            }
            button {
                r#type: "button",
                class: "mobile-deck-new",
                "data-testid": "tab-new",
                "aria-label": "New terminal — same folder & server",
                title: "New terminal in this folder",
                onclick: move |_| on_new_tab.call(()),
                svg { width: "18", height: "18", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2.5", stroke_linecap: "round",
                    path { d: "M12 5v14M5 12h14" }
                }
            }
            button {
                r#type: "button",
                class: if badge.fraction { "mobile-deck-count mobile-deck-count--fraction" } else { "mobile-deck-count" },
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
