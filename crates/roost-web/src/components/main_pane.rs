//! The main pane: the persistent terminal deck behind every terminal, file and
//! search route, the file/search overlays above it, and the compact drawer's
//! dim. Ports `apps/web/src/components/MainPane.tsx`; mounted by `app::RouteContent`
//! for every `ServedSurface::MainPane` route, so crossing /s ↔ /file ↔ /search
//! keeps one pane and one deck mounted and only flips the deck host's
//! visibility. Depends on `route_session`, `dead_route_safety_net`, the store.

mod safety_net;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::sync::SyncDomain;

use crate::components::layout::window_size::use_is_compact;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::route_session::{active_open_session_for_route, is_terminal_route};
use crate::router_state::use_location;
use crate::routes::Route;
use crate::terminal_href::terminal_href;

/// Which overlay, if any, covers the deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainPaneOverlay {
    /// The deck is the surface.
    None,
    /// `/file/…`: the file viewer sheet.
    File,
    /// `/search`: the global search page.
    Search,
}

impl MainPaneOverlay {
    /// The overlay a route shows above the deck.
    pub fn for_route(route: &Route) -> Self {
        match route {
            Route::File { .. } => Self::File,
            Route::Search => Self::Search,
            _ => Self::None,
        }
    }
}

/// What the pane needs from the store, read in one borrow.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneReading {
    open_session_id: Option<String>,
    visit: Option<(String, String, String, String)>,
    sidebar_open: bool,
}

/// The main pane for `route`.
#[component]
pub fn MainPane(route: Route) -> Element {
    let pump = use_store();
    let path = use_location();
    let compact = use_is_compact();
    let overlay = MainPaneOverlay::for_route(&route);
    let reading = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let open = active_open_session_for_route(store, &BrowserWorkerPaths, &route);
        PaneReading {
            open_session_id: open.map(|session| session.id.as_str().to_owned()),
            visit: open.map(|session| {
                (
                    session.id.as_str().to_owned(),
                    session.worker_fp.as_str().to_owned(),
                    session
                        .spawn_cwd
                        .clone()
                        .unwrap_or_else(|| session.cwd.clone()),
                    terminal_href(store, session),
                )
            }),
            sidebar_open: store.ui.sidebar_open,
        }
    };
    safety_net::use_dead_route_safety_net(
        &pump,
        path,
        route.clone(),
        reading.open_session_id.clone(),
        is_terminal_route(&route),
        pump.core()
            .borrow()
            .store()
            .sync
            .domain_is_ready(SyncDomain::Terminal),
    );
    let visit_pump = pump.clone();
    use_effect(use_reactive((&reading.visit,), move |(visit,)| {
        remember_visit(&visit_pump, visit)
    }));

    let overlay_active = overlay != MainPaneOverlay::None;
    let dim_pump = pump.clone();
    rsx! {
        div { style: "flex: 1; display: flex; flex-direction: column; overflow: hidden; position: relative;",
            if compact && reading.sidebar_open {
                div {
                    "data-testid": "main-pane-sidebar-dim",
                    "aria-hidden": "true",
                    style: "position: absolute; inset: 0; background: transparent; z-index: 48; cursor: pointer;",
                    onclick: move |_| dim_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer)),
                }
            }
            match overlay {
                MainPaneOverlay::File => rsx! {
                    crate::components::file_viewer::FileViewer { route: route.clone() }
                },
                MainPaneOverlay::Search => rsx! {
                    crate::components::global_search::GlobalSearchOverlay {}
                },
                MainPaneOverlay::None => rsx! {},
            }
            div {
                "data-testid": "main-pane-deck-host",
                style: deck_host_style(overlay_active),
                "aria-hidden": overlay_active.then_some("true"),
                crate::components::deck::terminal_deck::TerminalDeck {
                    active_session_id: reading.open_session_id.clone(),
                    surface_visible: !overlay_active,
                }
            }
        }
    }
}

/// The deck host's style: always laid out, hidden and pointer-transparent under
/// an overlay so park geometry stays truthful and nothing remounts on return.
pub fn deck_host_style(overlay_active: bool) -> String {
    let (visibility, pointer_events) = if overlay_active {
        ("hidden", "none")
    } else {
        ("visible", "auto")
    };
    format!(
        "position: absolute; inset: 0; display: flex; flex-direction: column; \
         visibility: {visibility}; pointer-events: {pointer_events};"
    )
}

/// Remember a LIVE terminal (browser-local) whenever the viewed session or its
/// folder changes, so a dead route never overwrites a good memory.
fn remember_visit(pump: &crate::pump::Pump, visit: Option<(String, String, String, String)>) {
    let Some((session_id, worker_fp, folder, href)) = visit else {
        return;
    };
    pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RememberVisit {
        session_id,
        worker_fp,
        folder,
        href,
    }));
}
