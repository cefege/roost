//! The Dioxus application: its route grammar, its components, its platform
//! seam, and the root that owns the client core.
//!
//! Everything Dioxus-shaped lives here and nowhere else. `roost-client-core` has
//! no framework and `roost-web-terminal` has no framework either, and the rule
//! that keeps the three apart is that a component may read the store and call
//! `handle`, and nothing else — a component that reaches a socket, a fetch or a
//! DOM node directly is a rule the client core cannot see.
//!
//! The store's `revision` is the only thing a component subscribes to. A
//! component that wants to know "did this session's grid move" reads that
//! session's `frame_revision` and compares it; it never polls, and it never
//! subscribes to a field the core does not fold.
//!
//! `app` is the router: it turns a path into a surface and gates that surface
//! behind the browser's access state. `routes` is the grammar it turns, and it
//! is the only URL grammar in the tree.

pub mod app;
pub mod components;
pub mod dead_route_safety_net;
pub mod display_format;
pub mod input_nav;
pub mod keyboard_shortcuts;
#[cfg(target_arch = "wasm32")]
pub mod keyboard_shortcuts_dom;
pub mod machine_actions;
pub mod motion;
pub mod new_terminal_target;
pub mod platform;
pub mod pump;
pub mod route_session;
pub mod router_state;
pub mod routes;
pub mod session_actions;
pub mod session_naming;
pub mod syntax_lite;
pub mod terminal_file_link;
pub mod terminal_href;
pub mod theme;
pub mod ui_bridge;
pub mod voice;

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientCore;

/// Install this tab's tracing subscriber, once.
///
/// `try_init` rather than `init`: a subscriber can already be present when the
/// bundle is loaded into a harness that installed one, and a second `init`
/// panics inside the entry point where nothing can catch it.
///
/// The writer is the browser console through a `MakeWriter` of this crate's
/// own, because `tracing-subscriber`'s default writer is `io::stderr` and a
/// `wasm32-unknown-unknown` build has nowhere for that to go: the events would be
/// formatted and dropped, which is a log line that looks present and is not.
/// `roost_observability::init()` is the shared owner of this and should replace
/// it as soon as `xtask/src/crate_dag.rs` allows `roost-web → roost-observability`.
pub fn install_tracing() {
    struct BrowserConsole;

    impl std::io::Write for BrowserConsole {
        fn write(&mut self, line: &[u8]) -> std::io::Result<usize> {
            let text = String::from_utf8_lossy(line);
            web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(text.trim_end()));
            Ok(line.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // This crate names an explicit target on every event — `carriers`, `door`,
    // `auth`, `terminal`, 34 of them — and an `EnvFilter` directive matches a
    // target by prefix. `roost_web=info` therefore enabled info for the crate
    // ROOT and nothing else, and every named target fell to the bare `warn`
    // default and was dropped: the browser emitted no carrier, door or sync
    // line at all, which is what made a page-side stop unnameable.
    //
    // The default is `warn`, because the only reader is an operator reading a
    // console. `RUST_LOG` overrides it, which is how a developer widens it.
    let default = "warn";
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    // `without_time`: the default timer reads `SystemTime::now()`, which panics
    // on wasm32-unknown-unknown, so the first event that passed the filter
    // aborted the tab (`RuntimeError: unreachable`). The console stamps lines.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .json()
        .with_writer(std::sync::Mutex::new(BrowserConsole))
        .try_init();
}

pub use app::{Gate, Surface, surface_for};
pub use routes::Route;

/// The application root.
///
/// Owns the core and hands it to the router. The core is created here rather
/// than per route so a navigation cannot produce a second store: a client with
/// two stores has two recovery cursors and applies the same event twice.
///
/// `Rc<RefCell<_>>` rather than the core itself, because context values are read
/// by clone and a `Clone` core would be two cores with two recovery cursors. The
/// cell is the owner and there is exactly one of them, created before the first
/// component can ask for a store.
#[component]
pub fn App() -> Element {
    // Provided, not passed. The core is ONE per application and every component
    // below reads it, so a prop would be a parameter threaded through every
    // level of the tree to reach the two components that read it — and a prop
    // also has to be `PartialEq`, which a state machine with a chunk assembler in
    // it cannot be.
    //
    // The pump is built once, in the root scope that owns the revision signal,
    // and started in the same hook: the core it drives is the provided one.
    let revision = use_signal(|| 0_u64);
    // The persisted palette lands before the first component paints (v2
    // `main.tsx` applies it before `render`).
    use_hook(theme::apply_stored_theme);
    let pump = use_hook(|| pump::start_pump(Rc::new(RefCell::new(build_core())), revision));
    use_context_provider(|| pump.core());
    use_context_provider(|| pump.clone());
    use_context_provider(components::terminal::pane_registry::PaneRegistry::default);
    components::layout::window_size::WindowSize::provide();
    motion::resize_drag::ResizeDrag::provide();
    keyboard_shortcuts::ShortcutOverlays::provide();
    rsx! {
        components::app_error_boundary::AppErrorBoundary { app::GatedApp {} }
    }
}

/// The client core over this browser's platform.
///
/// `Rc`, not `Arc`: the core is one state machine on one thread, and a client
/// that could be shared across threads would need a lock over state that has
/// exactly one writer. A host that wants the core on a task confines it to that
/// task and sends results outward.
///
/// No tab id yet: the pump claims one per document (`platform::tab_id`) before
/// its first transport, so a duplicated tab cannot share its sibling's.
fn build_core() -> ClientCore {
    let storage = platform::LocalStorageKeyValueStore::new();
    // A television three metres from the sofa cannot read a 14 px cell, and
    // the size has to be decided HERE: the core is built before the app root
    // exists, and the first pane measures against the store it hands over.
    let defaults = roost_client_core::store::prefs::PrefDefaults {
        term_font_px: if crate::input_nav::device_tv_mode_active(&storage) {
            roost_client_core::store::prefs::terminal_font::TERMINAL_FONT_TV_DEFAULT_PX
        } else {
            roost_client_core::store::prefs::terminal_font::TERMINAL_FONT_DEFAULT_PX
        },
    };
    ClientCore::new(
        Rc::new(platform::BrowserClock::new()),
        Rc::new(storage),
        "",
        &defaults,
    )
}
