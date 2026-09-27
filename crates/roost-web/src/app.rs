//! The router: which surface a path names, and the gate every protected surface
//! waits behind. Ported from `apps/web/src/App.tsx`'s `RootShell` switch and its
//! route table, with `routes::Route` as the only URL grammar.
//!
//! The mapping is a total function from `Route` to `Surface` and it is pure, so
//! "which URL renders what" is one auditable table rather than a match buried in
//! a component. `AppShell` draws the chrome; the gate decides whether it is
//! drawn at all.
//!
//! THE GATE IS THE CLOSED DEFAULT. `BrowserAccessState::Checking` is where the
//! store starts and where an auth-generation advance puts it back, so a client
//! that has not heard from the coordinator renders the checking screen and
//! nothing else. A surface reachable before the credential is known is a surface
//! readable by a tab this coordinator has not authorized.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::store::BrowserAccessState;

use crate::components::access_gate;
use crate::components::layout::AppShell;
use crate::routes::Route;

/// Which surface a route names.
///
/// Every variant is a decision a reader can audit, and the ones this build does
/// not serve are NAMED as such rather than silently redirected. v2 sends an
/// unmatched path to `/`; doing that here would make a link to a surface that is
/// not in this build look like it worked, which is the one outcome a router
/// exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Surface {
    /// A surface this build serves.
    Served(ServedSurface),
    /// A path the grammar recognises whose surface is not in this build.
    NotServed {
        /// The path as the grammar read it.
        path: String,
    },
    /// A path the grammar does not recognise at all.
    NotFound {
        /// The path as it was typed.
        path: String,
    },
}

/// The surfaces this build serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedSurface {
    /// `/` — the session list and the workbench.
    Home,
}

impl ServedSurface {
    /// The `data-testid` the Playwright specs address this surface by.
    pub const fn test_id(self) -> &'static str {
        match self {
            Self::Home => "home-landing",
        }
    }
}

/// Resolve a route to the surface that renders it.
///
/// Total by construction: every `Route` variant is named, so adding a route to
/// the grammar is a compile error here until someone decides what it renders.
/// A route that appears in the grammar and nowhere else is a link that goes
/// nowhere, and the compiler is the cheapest place to notice.
pub fn surface_for(route: &Route) -> Surface {
    match route {
        Route::Home => Surface::Served(ServedSurface::Home),
        Route::Session { .. }
        | Route::Terminal { .. }
        | Route::Workspace { .. }
        | Route::Settings { .. }
        | Route::Pair
        | Route::Help
        | Route::Design
        | Route::File { .. }
        | Route::Browse { .. }
        | Route::Search => Surface::NotServed {
            path: route.to_path(),
        },
        Route::Unknown { path } => Surface::NotFound { path: path.clone() },
    }
}

/// What the access gate shows, and what it lets through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Waiting for the coordinator to say whether this device key is trusted.
    Checking,
    /// The key is not trusted. No protected surface renders.
    Unauthorized,
    /// The key is trusted; the router's surface renders.
    Authorized,
}

impl Gate {
    /// The gate for a browser access state.
    ///
    /// `Checking` is the default, so a new client and a client whose credential
    /// was just invalidated render the same thing: nothing protected.
    pub const fn for_state(state: BrowserAccessState) -> Self {
        match state {
            BrowserAccessState::Checking => Self::Checking,
            BrowserAccessState::Unauthorized => Self::Unauthorized,
            BrowserAccessState::Authorized => Self::Authorized,
        }
    }

    /// Whether the router's surface is allowed to render.
    pub const fn admits_surface(self) -> bool {
        matches!(self, Self::Authorized)
    }
}

/// The gated application: the gate first, then the shell and the route surface.
///
/// The gate is decided here and nowhere else. A component reached through this
/// one has already been decided authorized, which is why no component below
/// re-reads `browser_access_state` to decide whether to show itself.
///
/// THE STATE IS READ DURING RENDER, NOT memoized. Nothing drives
/// `ClientCore::handle` in this build — the host pump that turns Sync frames and
/// RPC results into client events is not here — so the store does not change and
/// the gate holds at `Checking` until that pump lands. Reading during render
/// means the gate is correct the moment a pump exists, with nothing to revisit; a
/// hook-held snapshot would freeze the first answer and need a second mechanism
/// to un-freeze it.
#[component]
pub fn GatedApp() -> Element {
    let core = use_context::<Rc<RefCell<ClientCore>>>();
    match Gate::for_state(read_access(&core)) {
        Gate::Checking => rsx! { access_gate::CheckingScreen {} },
        Gate::Unauthorized => rsx! { access_gate::UnauthorizedScreen {} },
        Gate::Authorized => rsx! { AuthorizedShell {} },
    }
}

/// The shell, with the surface the current path names in its editor slot.
#[component]
fn AuthorizedShell() -> Element {
    let path = current_path();
    let surface = surface_for(&Route::parse(&path));
    rsx! {
        AppShell {
            path,
            children: rsx! { RouteContent { surface } },
        }
    }
}

/// The surface, or the honest statement of why there is not one.
#[component]
fn RouteContent(surface: Surface) -> Element {
    match surface {
        Surface::Served(ServedSurface::Home) => rsx! {
            crate::components::home::HomeLanding { apple_keyboard: apple_keyboard() }
        },
        Surface::NotServed { path } => {
            rsx! { crate::components::not_served::NotServed { path } }
        }
        Surface::NotFound { path } => {
            rsx! { crate::components::not_served::NotFound { path } }
        }
    }
}

/// Whether this platform's keyboard uses the Command key, which is what decides
/// the glyph a shortcut is advertised with.
///
/// Read at paint time rather than tracked: the answer cannot change while the
/// document is open, so a subscription here would be a listener whose only
/// effect is to fire once.
#[cfg(target_arch = "wasm32")]
fn apple_keyboard() -> bool {
    web_sys::window().is_some_and(|window| window.navigator().user_agent().contains("Mac"))
}

/// A native build reports no platform, and `Ctrl` is the reading that is wrong
/// on fewer machines than `⌘`.
#[cfg(not(target_arch = "wasm32"))]
fn apple_keyboard() -> bool {
    false
}

/// The path the document is showing.
///
/// Read during render and cached nowhere, so there is no copy of the address
/// that can disagree with the address bar.
#[cfg(target_arch = "wasm32")]
fn current_path() -> String {
    crate::platform::location::current_location()
}

/// A native build has no address bar, so the router reads the root. That keeps
/// `cargo test -p roost-web` exercising the same components a browser runs
/// rather than a second set that exists only natively.
#[cfg(not(target_arch = "wasm32"))]
fn current_path() -> String {
    "/".to_string()
}

/// The browser access state, read through the core.
fn read_access(core: &Rc<RefCell<ClientCore>>) -> BrowserAccessState {
    core.borrow().store().browser_access_state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_renders_the_home_surface() {
        assert_eq!(
            surface_for(&Route::parse("/")),
            Surface::Served(ServedSurface::Home)
        );
    }

    #[test]
    fn every_grammar_route_but_the_root_names_itself_in_the_not_served_panel() {
        // The panel must show the path the READER typed, so the string is built
        // from `to_path` and never from a per-route literal that could drift.
        let mut checked = 0_usize;
        for route in Route::ALL {
            if *route == Route::Home {
                continue;
            }
            checked += 1;
            assert_eq!(
                surface_for(route),
                Surface::NotServed {
                    path: route.to_path()
                },
                "{route:?}"
            );
        }
        assert_eq!(checked, Route::ALL.len() - 1);
    }

    #[test]
    fn a_session_route_is_not_served_rather_than_redirected_home() {
        // The defect this rules out: `/s/abc` rendering the session list, which a
        // reader would read as "my terminal is gone".
        assert_eq!(
            surface_for(&Route::parse("/s/abc")),
            Surface::NotServed {
                path: "/s/abc".into()
            }
        );
    }

    #[test]
    fn a_legacy_workspace_route_keeps_its_channel_in_the_named_path() {
        // The legacy form is one route with an optional channel, so the panel has
        // to show `/w/ws1/t/ch1` and not the shortened `/w/ws1` a reader did not
        // type.
        assert_eq!(
            surface_for(&Route::parse("/w/ws1/t/ch1")),
            Surface::NotServed {
                path: "/w/ws1/t/ch1".into()
            }
        );
    }

    #[test]
    fn an_unrecognised_path_is_not_found_rather_than_not_served() {
        // The two read differently to a reader: one names a surface this build
        // lacks, the other a URL that was never a route. Collapsing them would
        // make a typo look like a missing feature.
        assert_eq!(
            surface_for(&Route::parse("/nope")),
            Surface::NotFound {
                path: "/nope".into()
            }
        );
    }

    #[test]
    fn a_query_string_is_not_part_of_the_path_a_reader_is_shown() {
        // `Route::parse` strips the query, so the panel shows the path without
        // it. A panel reading `?session=abc` would suggest the query is part of
        // the address when it is a parameter of one.
        assert_eq!(
            surface_for(&Route::parse("/search?q=abc")),
            Surface::NotServed {
                path: "/search".into()
            }
        );
    }

    #[test]
    fn the_gate_is_closed_until_the_coordinator_answers() {
        assert_eq!(
            Gate::for_state(BrowserAccessState::Checking),
            Gate::Checking
        );
        assert_eq!(
            Gate::for_state(BrowserAccessState::Unauthorized),
            Gate::Unauthorized
        );
        assert!(!Gate::Checking.admits_surface());
        assert!(!Gate::Unauthorized.admits_surface());
        assert!(Gate::Authorized.admits_surface());
    }
}
