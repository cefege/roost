//! The deck's subscription to the store, and what the two pump hooks mean.
//!
//! `TerminalDeck` derives its whole frame from `pump.core()` during render, so
//! a store mutation that changes no route prop — a tab click landing as
//! `DeckIntent::SelectTab` — reaches the reader only if the deck subscribed to
//! the revision. It read the pump WITHOUT subscribing, so `data-active` on a
//! pane tab never flipped and the compact badge kept counting the tab the
//! reader was already on.
//!
//! These build a real `Pump` over a real `ClientCore` and mount the real
//! component, so the deck is exercised through the same host the browser uses.
//! The observable is the ROUTE the deck asks for: `use_deck_navigation` reads
//! `store.deck.navigation()` during render, so a deck that never re-rendered
//! never learns there is a route to follow.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_client_core::deck::{DeckFolder, DeckIntent};
use roost_client_core::{ClientCore, ClientEvent};
use roost_web::components::deck::terminal_deck::TerminalDeck;
use roost_web::components::layout::window_size::WindowSize;
use roost_web::motion::resize_drag::ResizeDrag;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::{Pump, use_pump, use_store};
use roost_web::router_state::provide_router;

const FIRST: &str = "00000000-0000-4000-8000-00000000000a";
const SECOND: &str = "00000000-0000-4000-8000-00000000000b";
const FOLDER: &str = "roost/folder-a";

/// How many render-then-effect rounds one mutation is given. A deck render
/// dispatches its own observation of the frame it drew, so one mutation is
/// answered by more than one round; the bound stops a cycle from hanging the
/// test rather than from failing it.
const SETTLE_ROUNDS: usize = 8;

thread_local! {
    /// The pump `deck_root` built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
    /// Every path the deck asked the router to move to.
    static NAVIGATED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// How many times each probe rendered.
    static STORE_PROBE: Cell<u32> = const { Cell::new(0) };
    static PUMP_PROBE: Cell<u32> = const { Cell::new(0) };
}

/// The two open sessions a folder holds, oldest first.
fn folder() -> DeckFolder {
    DeckFolder {
        folder_key: FOLDER.to_owned(),
        live_session_ids: vec![FIRST.to_owned(), SECOND.to_owned()],
    }
}

/// The root `VirtualDom::new` wants: `Pump::new` reads a `Signal`, and only a
/// real scope owns one.
fn deck_root() -> Element {
    use_context_provider(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-deck"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    provide_router(
        Signal::new(String::new()),
        EventHandler::new(|path: String| {
            NAVIGATED.with(|navigated| navigated.borrow_mut().push(path));
        }),
    );
    WindowSize::provide();
    ResizeDrag::provide();
    rsx! {
        TerminalDeck { active_session_id: None, surface_visible: true }
        StoreProbe {}
        PumpProbe {}
    }
}

/// Counts its own renders, reading the store the way a surface does.
#[component]
fn StoreProbe() -> Element {
    let pump = use_store();
    let open = {
        let core = pump.core();
        let core = core.borrow();
        core.store().spotlight.visible_pane_count()
    };
    STORE_PROBE.with(|count| count.set(count.get() + 1));
    rsx! { div { "store-probe {open}" } }
}

/// The same read with no subscription, which is the defect this file names.
#[component]
fn PumpProbe() -> Element {
    let pump = use_pump();
    let open = {
        let core = pump.core();
        let core = core.borrow();
        core.store().spotlight.visible_pane_count()
    };
    PUMP_PROBE.with(|count| count.set(count.get() + 1));
    rsx! { div { "pump-probe {open}" } }
}

/// A mounted deck and the pump it reads, both alive together: the pump's
/// revision signal is owned by the scope the dom created.
struct Deck {
    pump: Pump,
    /// Kept alive so the pump's revision signal keeps the scope that owns it.
    dom: VirtualDom,
}

impl Deck {
    /// A tab click as the deck's chrome raises it: a store mutation that
    /// changes no route prop and asks for a route.
    fn select_tab(&mut self, session_id: &str) {
        self.dom.in_runtime(|| {
            self.pump.dispatch(ClientEvent::Deck(DeckIntent::SelectTab {
                folder: folder(),
                session_id: session_id.to_owned(),
                compact: false,
            }));
        });
        self.settle();
    }

    /// Run the work a mutation queued, in the order the browser runs it: the
    /// scopes the revision marked dirty re-render, and the effects those
    /// renders queued run after. `rebuild_in_place` is deliberately NOT used
    /// here, because it re-creates every scope from the root and wipes the
    /// state a mounted deck had already accumulated.
    fn settle(&mut self) {
        for _ in 0..SETTLE_ROUNDS {
            self.dom.render_immediate(&mut NoOpMutations);
            self.dom.process_events();
        }
    }

    fn navigated(&self) -> Vec<String> {
        NAVIGATED.with(|navigated| navigated.borrow().clone())
    }
}

/// A deck over an empty store, with every context the component reads.
fn mounted_deck() -> Deck {
    NAVIGATED.with(|navigated| navigated.borrow_mut().clear());
    STORE_PROBE.with(|count| count.set(0));
    PUMP_PROBE.with(|count| count.set(0));
    let mut dom = VirtualDom::new(deck_root);
    dom.rebuild_in_place();
    let mut deck = Deck {
        pump: BUILT.with(|built| built.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always builds a \
             pump; an empty slot means the render pass never happened",
        ),
        dom,
    };
    deck.settle();
    deck
}

/// THE REGRESSION. A tab click changes the store and the route the deck should
/// show, and changes NO prop the deck was handed. Before the deck subscribed,
/// `use_pump` left it parked on the frame it first derived, so the reader's
/// click moved the store and nothing else: the tab wrapper kept
/// `data-active="false"` and the compact badge kept counting the old tab.
#[test]
fn a_deck_tab_click_reaches_the_deck_without_a_prop_changing() {
    let mut deck = mounted_deck();
    assert!(
        deck.navigated().is_empty(),
        "mounting follows no route: a request that predates this deck is not replayed"
    );

    deck.select_tab(SECOND);

    assert_eq!(
        deck.navigated(),
        vec![format!("/s/{SECOND}")],
        "a store mutation that changes no route prop must still reach the deck, \
         or the tab the reader clicked never becomes the tab the deck shows"
    );
}

/// WHAT `use_store` BUYS, and why `use_pump` is not a cheaper spelling of it.
/// A subscribed reader repaints when the store moves; an unsubscribed one
/// reading the same `Rc<RefCell<ClientCore>>` keeps the value it first read.
/// Both run in the same pass, over the same store, so the difference is the
/// revision read and nothing else.
#[test]
fn only_the_subscribed_reader_repaints_when_the_store_moves() {
    let mut deck = mounted_deck();
    let store_renders = STORE_PROBE.with(Cell::get);
    let pump_renders = PUMP_PROBE.with(Cell::get);
    assert!(
        store_renders > 0 && pump_renders > 0,
        "both probes render on mount"
    );

    deck.select_tab(SECOND);

    assert!(
        STORE_PROBE.with(Cell::get) > store_renders,
        "use_store reads the revision, so a store mutation repaints its caller; \
         the count is not pinned to one because the deck's own observation of \
         the frame it just drew is a second mutation in the same settle"
    );
    assert_eq!(
        PUMP_PROBE.with(Cell::get),
        pump_renders,
        "use_pump is the unsubscribed read; a component that renders store \
         state through it is one store mutation behind, which is the defect \
         this file exists to pin"
    );
}
