//! The Dioxus application: its route grammar, its platform seam, and the root
//! component that owns the client core.
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

pub mod platform;
pub mod routes;

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::KeyValueStore as _;

/// Install this tab's tracing subscriber, once.
///
/// `try_init` rather than `init`: a subscriber can already be present when the
/// bundle is loaded into a harness that installed one, and a second `init`
/// panics inside the entry point where nothing can catch it.
///
/// The writer is the browser console through a `MakeWriter` of this crate's
/// own, because `tracing-subscriber`'s default writer is `io::stderr` and a
/// `wasm32-unknown-unknown` build has nowhere for that to go: the events would
/// be formatted and dropped, which is a log line that looks present and is not.
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

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("roost_web=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_writer(std::sync::Mutex::new(BrowserConsole))
        .try_init();
}

pub use routes::Route;

/// The local-storage key the tab identity is kept under.
const TAB_ID_KEY: &str = "roost.tabId";

/// The application root.
///
/// Owns the core and hands it to the router. The core is created here rather
/// than per route so a navigation cannot produce a second store: a client with
/// two stores has two recovery cursors and applies the same event twice.
///
/// The root renders nothing yet, and that is a tracked gap rather than an
/// oversight: Dioxus 0.7's router is a typed route enum whose variants each need
/// the component they route to, and those components are the next wave's work.
/// The URL grammar in `routes` is already the contract they will be built from.
#[component]
pub fn App() -> Element {
    // `Rc<RefCell<_>>` rather than the core itself: a Dioxus hook's value has to
    // be `Clone`, and a `Clone` core would be two cores with two recovery
    // cursors. The cell is the owner and there is exactly one of them, created
    // before the first component can ask for a store.
    let _core = use_hook(|| Rc::new(RefCell::new(build_core())));
    // Dioxus 0.7's router is a TYPED route enum (`#[derive(Routable)]`), not a
    // table of path strings, and each variant needs the component it routes to.
    // That enum arrives with the component slices; until then the root renders
    // nothing rather than a placeholder that would look like a working page.
    // `routes::Route` is already the path grammar those links are built from.
    rsx! {}
}

/// The client core over this browser's platform.
///
/// `Rc`, not `Arc`: the core is one state machine on one thread, and a client
/// that could be shared across threads would need a lock over state that has
/// exactly one writer. A host that wants the core on a task confines it to that
/// task and sends results outward.
fn build_core() -> ClientCore {
    ClientCore::new(
        Rc::new(platform::BrowserClock::new()),
        Rc::new(platform::LocalStorageKeyValueStore::new()),
        &tab_id(),
    )
}

/// The tab identity every transport and every Connect call presents.
///
/// Persisted rather than minted per load, because a reload that mints a new id is
/// a tab the coordinator's `audit_log` rows cannot join back together. v2
/// arbitrated duplicates with a Web Lock and a `BroadcastChannel`; until the auth
/// ceremony slice adds that, a second tab on one origin shares an id rather than
/// minting two.
fn tab_id() -> String {
    let store = platform::LocalStorageKeyValueStore::new();
    if let Some(existing) = store.get(TAB_ID_KEY)
        && !existing.is_empty()
    {
        return existing;
    }
    let minted = format!("tab-{:x}", js_sys::Math::random().to_bits());
    store.set(TAB_ID_KEY, &minted);
    minted
}
