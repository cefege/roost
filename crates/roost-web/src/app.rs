//! The router: which surface a path names, the gate every protected surface
//! waits behind, and the authorized shell's context (router, overlays,
//! keyboard). Ported from `apps/web/src/App.tsx`'s `RootShell` and route table,
//! with `routes::Route` as the only URL grammar; mounted by `crate::App`.
//!
//! THE GATE IS THE CLOSED DEFAULT. `BrowserAccessState::Checking` is where the
//! store starts and where an auth-generation advance puts it back, so a client
//! that has not heard from the coordinator renders the checking screen and
//! nothing protected. `surface_for` is a total, pure function from `Route` to
//! `Surface`, so "which URL renders what" is one auditable table.

use dioxus::prelude::*;
use roost_client_core::store::BrowserAccessState;

use crate::components::access_gate;
use crate::components::layout::AppShell;
use crate::components::main_pane::MainPane;
use crate::router_state;
use crate::routes::Route;

/// Which surface a route names. Surfaces this build does not serve are NAMED
/// (`NotServed`) rather than redirected, so a link to a missing surface cannot
/// look like it worked.
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
    /// `/` — the landing page inside the workbench.
    Home,
    /// Every terminal route plus `/file/…` and `/search`: ONE surface, so moving
    /// between them keeps the pane and its deck mounted (v2's path-array route).
    MainPane,
    /// `/design` — the design-system gallery, outside the workbench.
    Design,
}

impl ServedSurface {
    /// Whether the surface is drawn inside `AppShell` (v2 nests every route but
    /// `/pair` and `/design` under the shell route).
    pub const fn in_shell(self) -> bool {
        !matches!(self, Self::Design)
    }
}

/// Resolve a route to the surface that renders it. Total by construction.
pub fn surface_for(route: &Route) -> Surface {
    match route {
        Route::Home => Surface::Served(ServedSurface::Home),
        Route::Session { .. }
        | Route::Terminal { .. }
        | Route::Workspace { .. }
        | Route::File { .. }
        | Route::Search => Surface::Served(ServedSurface::MainPane),
        Route::Design => Surface::Served(ServedSurface::Design),
        Route::Settings { .. } | Route::Pair | Route::Help | Route::Browse { .. } => {
            Surface::NotServed {
                path: route.to_path(),
            }
        }
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

/// The gated application. The router context is provided ABOVE the gate so an
/// access transition never remounts it; the state is read during render with
/// the pump's revision (`use_store`), never memoized.
#[component]
pub fn GatedApp() -> Element {
    let core = crate::pump::use_store().core();
    let path = router_state::use_path_signal();
    let on_navigate = router_state::navigation_handler(path);
    router_state::provide_router(path, on_navigate);
    #[cfg(target_arch = "wasm32")]
    install_document_input(crate::pump::use_pump(), path);
    #[cfg(target_arch = "wasm32")]
    {
        let font_px = core.borrow().store().prefs.term_font_px;
        let mut applied = use_signal(|| 0_u32);
        if *applied.peek() != font_px {
            crate::keyboard_shortcuts_dom::apply_term_font_size(font_px);
            applied.set(font_px);
        }
    }
    let access = core.borrow().store().browser_access_state;
    match Gate::for_state(access) {
        Gate::Checking => rsx! { access_gate::CheckingScreen {} },
        Gate::Unauthorized => rsx! { access_gate::UnauthorizedScreen { on_navigate } },
        Gate::Authorized => rsx! {
            AuthorizedShell { path }
            AuthorizedOverlays {}
        },
    }
}

/// The document-wide input owners, installed once for the life of the root:
/// the TV/pad modality on `<html>`, D-pad spatial navigation, and the global
/// keyboard router (v2 `main.tsx` `applyTvMode`/`applyPadMode` and `App`'s
/// `onMount`).
#[cfg(target_arch = "wasm32")]
fn install_document_input(pump: crate::pump::Pump, path: Signal<String>) {
    use crate::input_nav::{
        NavModality, apply_nav_modality, install_spatial_navigation, load_nav_modality,
    };

    let modality = use_hook(|| {
        let modality = load_nav_modality(&crate::platform::LocalStorageKeyValueStore::new());
        apply_nav_modality(&modality);
        Signal::new(modality)
    });
    use_context_provider(|| modality);
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    use_hook(move || {
        let _: Signal<NavModality> = modality;
        std::rc::Rc::new((
            install_spatial_navigation(modality),
            crate::keyboard_shortcuts_dom::install_keyboard_shortcuts(
                pump, overlays, path, modality,
            ),
        ))
    });
}

/// The overlay hosts an authorized browser mounts once identity discovery has
/// settled (v2 `RootShell`'s `coordinatorDiscovered` group).
#[component]
fn AuthorizedOverlays() -> Element {
    let pump = crate::pump::use_store();
    if pump.core().borrow().store().coord_identity.is_none() {
        return rsx! {};
    }
    rsx! {
        crate::components::rename_dialog::RenameDialogHost {}
    }
}

/// The surface the current path names, inside the shell when it belongs there.
#[component]
fn AuthorizedShell(path: Signal<String>) -> Element {
    let route = Route::parse(&path());
    let surface = surface_for(&route);
    let in_shell = !matches!(surface, Surface::Served(served) if !served.in_shell());
    let content = rsx! { RouteContent { surface, route } };
    if in_shell {
        rsx! { AppShell { {content} } }
    } else {
        content
    }
}

/// The surface, or the honest statement of why there is not one.
#[component]
fn RouteContent(surface: Surface, route: Route) -> Element {
    let navigate = router_state::use_navigate();
    match surface {
        Surface::Served(ServedSurface::Home) => rsx! {
            crate::components::home::HomeLanding { apple_keyboard: apple_keyboard() }
        },
        Surface::Served(ServedSurface::MainPane) => rsx! { MainPane { route } },
        Surface::Served(ServedSurface::Design) => {
            rsx! { crate::components::design::DesignGallery { on_navigate: navigate } }
        }
        Surface::NotServed { path } => rsx! { crate::components::not_served::NotServed { path } },
        Surface::NotFound { path } => {
            rsx! { crate::components::not_served::NotFound { path, on_navigate: navigate } }
        }
    }
}

/// Whether this platform's keyboard uses the Command key (decides the glyph a
/// shortcut is advertised with). An unknown platform reads as `Ctrl`.
fn apple_keyboard() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::platform::browser_platform::browser_platform()
            == crate::platform::browser_platform::BrowserPlatform::MacOs
    }
    #[cfg(not(target_arch = "wasm32"))]
    false
}
