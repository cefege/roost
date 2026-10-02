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
#[cfg(target_arch = "wasm32")]
use crate::input_nav::NavModality;
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
    /// `/settings` and `/settings/:pane` — the settings shell, which owns the
    /// rail itself because the rail is route state and not chrome.
    Settings,
    /// `/pair` — the pairing ceremony, outside the shell.
    Pair,
    /// `/browse` and `/browse/:workerFp` — the machine and folder browser.
    Browse,
    /// `/help` — the help surface and the controller map.
    Help,
    /// `/design` — the design-system gallery, outside the workbench.
    Design,
}

impl ServedSurface {
    /// Whether the surface is drawn inside `AppShell` (v2 nests every route but
    /// `/pair` and `/design` under the shell route).
    pub const fn in_shell(self) -> bool {
        !matches!(self, Self::Design | Self::Pair)
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
        Route::Settings { .. } => Surface::Served(ServedSurface::Settings),
        Route::Pair => Surface::Served(ServedSurface::Pair),
        Route::Browse { .. } => Surface::Served(ServedSurface::Browse),
        Route::Help => Surface::Served(ServedSurface::Help),
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
    // The shell's composer reserve is a MEASUREMENT a dock publishes, and a
    // `thread_local` slot cannot re-render anything — reading only the slot
    // leaves the compact reserve a frame behind the dock that measured it. The
    // root owns the signal, so the root installs it. Installed above the gate
    // and above the shell, because a dock can mount in either.
    {
        use dioxus::prelude::*;
        let geometry = use_signal(Default::default);
        crate::components::terminal_chrome::composer_geometry::install_signal(geometry);
    }
    // A toast names a TARGET session, and the pane tab or sidebar row that
    // represents it is somewhere else entirely: `AuthorizedShell` and
    // `AuthorizedOverlays` are SIBLINGS, so a target provided inside the dock
    // cannot reach the rows that are meant to ring. Provided here, above both,
    // for the same reason the composer geometry is: the dock is one of two
    // children and it is not the one that reads it.
    {
        let _notify_target =
            crate::components::notifications::notify_target::NotifyTarget::provide();
    }
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
    let gated = match Gate::for_state(access) {
        Gate::Checking => rsx! { access_gate::CheckingScreen {} },
        // The unauthorized branch draws nothing of its own: the pairing
        // surface below draws the requester panel at every path, so an unpaired
        // reader at `/`, `/settings/devices` or `/search` gets the working
        // requester card (v2 `App.tsx:149-151`).
        Gate::Unauthorized => rsx! {},
        Gate::Authorized => rsx! {
            // The UI bridge is a SIBLING of the shell, not a child of it: its
            // report cadence and its command drain have to outlive every
            // surface, including `/design` and `/pair`, which render no
            // workbench at all.
            crate::ui_bridge::UiBridge {}
            AuthorizedShell { path }
            AuthorizedOverlays {}
        },
    };
    // The pairing surface sits OUTSIDE the gate, at a fixed place in this
    // template, so `checking → unauthorized → authorized` never remounts it:
    // the requester's recovery poll that finishes a ceremony after the gate
    // flipped, and the approver's code dialog, both live in its scope (v2
    // mounts both providers above the gate, `App.tsx:138-141`).
    rsx! {
        {gated}
        crate::components::pairing::PairSurface {}
    }
}

/// The document-wide input owners, installed once for the life of the root:
/// the TV/pad modality on `<html>`, D-pad spatial navigation, the Gamepad poll
/// and its shell, and the global keyboard router (v2 `main.tsx`
/// `applyTvMode`/`applyPadMode` and `App`'s `onMount`).
#[cfg(target_arch = "wasm32")]
fn install_document_input(pump: crate::pump::Pump, path: Signal<String>) {
    use crate::input_nav::{apply_nav_modality, install_spatial_navigation, load_nav_modality};

    let modality = use_hook(|| {
        let modality = load_nav_modality(&crate::platform::LocalStorageKeyValueStore::new());
        apply_nav_modality(&modality);
        Signal::new(modality)
    });
    use_context_provider(|| modality);
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    // The pad router's own hooks live in THIS scope, never inside a `use_hook`
    // initializer. A hook list is a `RefCell` borrowed for the whole of an
    // initializer, so a hook called from within one is a runtime panic rather
    // than a compile error (`dioxus_core::scope_context::Scope::use_hook`).
    // The install itself belongs INSIDE a `use_hook` too: `GatedApp` re-renders
    // on every store revision, and a poll installed per render registers a
    // second pair of listeners whose guard is dropped with that render, taking
    // the poll the memo still holds down with it. So all three guards are built
    // by ONE initializer, and it runs once.
    let pad_router = use_hook(|| Signal::new(crate::input_nav::PadActionRouter::new()));
    let pad_held = use_hook(|| Signal::new(crate::input_nav::PadHeld::default()));
    let compact = crate::components::layout::window_size::use_is_compact();
    let pad_shell = PadShellContext {
        pump: pump.clone(),
        overlays,
        route: path,
        modality,
        router: pad_router,
        held: pad_held,
        compact,
    };
    use_hook(move || {
        std::rc::Rc::new((
            install_spatial_navigation(modality),
            crate::keyboard_shortcuts_dom::install_keyboard_shortcuts(
                pump.clone(),
                overlays,
                path,
                modality,
            ),
            install_gamepad_router(pad_shell),
        ))
    });
}

/// What the controller router's shell half is built from, captured once for the
/// life of the document.
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct PadShellContext {
    pump: crate::pump::Pump,
    overlays: crate::keyboard_shortcuts::ShortcutOverlays,
    route: Signal<String>,
    modality: Signal<NavModality>,
    router: Signal<crate::input_nav::PadActionRouter>,
    held: Signal<crate::input_nav::PadHeld>,
    compact: bool,
}

/// The Gamepad poll and the shell half of the controller router, installed for
/// the life of the document.
///
/// The poll owns the button→intent mapping and the router owns what an intent
/// means; this only joins them to the store, the overlays, the deck and the
/// router, which is the wiring `apps/web/src/App.tsx` did at `onMount`.
///
/// NOT a component, and deliberately hook-free: its caller runs it from inside a
/// `use_hook` initializer, where a hook would re-borrow the list that
/// initializer already holds.
#[cfg(target_arch = "wasm32")]
fn install_gamepad_router(pad_shell: PadShellContext) -> crate::input_nav::GamepadSourceGuard {
    use crate::input_nav::install_gamepad_source;
    use crate::input_nav::pad_shell::ShellPadSurfaces;

    let PadShellContext {
        pump,
        overlays,
        route,
        modality,
        router,
        held,
        compact,
    } = pad_shell;
    let surfaces = std::rc::Rc::new(std::cell::RefCell::new(ShellPadSurfaces::new(
        pump, overlays, route, compact,
    )));
    install_gamepad_source(modality, held, move |actions| {
        let mut surfaces = surfaces.borrow_mut();
        crate::input_nav::dispatch_pad_actions(router, modality, &mut *surfaces, actions);
    })
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
        crate::components::notifications::NotificationDock {}
        crate::components::palette::CommandPalette {}
        crate::components::agents::queue_task_dialog::QueueTaskDialogHost {}
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
        Surface::Served(ServedSurface::Settings) => {
            rsx! { crate::components::settings::SettingsSurface { route } }
        }
        // `GatedApp` mounts the pairing surface once for the document; it draws
        // this page itself, so the route has nothing to add.
        Surface::Served(ServedSurface::Pair) => rsx! {},
        Surface::Served(ServedSurface::Browse) => {
            rsx! { crate::components::browse::BrowseSurface { route } }
        }
        Surface::Served(ServedSurface::Help) => rsx! { crate::components::help::HelpSurface {} },
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
