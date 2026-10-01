//! The WRITE half of the store-subscription invariant.
//!
//! `use_store` reads the pump's revision `Signal<u64>`, and that read is the
//! only thing that makes a component re-render when the store moves. A host
//! write that mutates the store without ending at the pump moves the store's
//! own counter and repaints nobody: the dock keeps drawing the frame it last
//! derived. `deck_store_subscription.rs` pins the READ half (a surface that
//! renders store state must subscribe); this pins the WRITE half (a surface
//! that mutates store state must repaint its subscribers).
//!
//! Both build a real `Pump` over a real `ClientCore` and mount a real
//! component in a real `VirtualDom`, so the observable is a render count and
//! not a restatement of the source.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};
use roost_web::components::notifications::store_write::write_store;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::{Pump, use_store};

/// The subject of the card every moving write here raises — the thing a reader
/// is looking at when it fails to appear.
const SUBJECT: &str = "session-a";

/// How many render-then-event rounds one write is given. The bound stops a
/// cycle from hanging the test rather than from failing it.
const SETTLE_ROUNDS: usize = 4;

thread_local! {
    /// The pump `root` built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
    /// How many times the probe rendered.
    static PROBE_RENDERS: Cell<u32> = const { Cell::new(0) };
}

/// The root `VirtualDom::new` wants: `Pump::new` reads a `Signal`, and only a
/// real scope owns one.
fn root() -> Element {
    use_context_provider(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("store-write"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    rsx! { StoreProbe {} }
}

/// Counts its own renders, reading the store the way a surface does.
#[component]
fn StoreProbe() -> Element {
    let pump = use_store();
    let live = {
        let core = pump.core();
        let core = core.borrow();
        core.store().toasts.len()
    };
    PROBE_RENDERS.with(|count| count.set(count.get() + 1));
    rsx! { div { "probe {live}" } }
}

/// A mounted pump and the dom that owns its revision signal.
struct Mounted {
    pump: Pump,
    /// Kept alive so the pump's revision signal keeps the scope that owns it.
    dom: VirtualDom,
}

impl Mounted {
    /// Run the work a write queued, the way the browser runs it: the scopes the
    /// revision marked dirty re-render, then the dom's effects run.
    fn settle(&mut self) {
        for _ in 0..SETTLE_ROUNDS {
            self.dom.render_immediate(&mut NoOpMutations);
            self.dom.process_events();
        }
    }

    fn renders(&self) -> u32 {
        PROBE_RENDERS.with(Cell::get)
    }

    /// Raise one card through the helper the notification cards use.
    fn raise_toast(&mut self) {
        let pump = self.pump.clone();
        self.dom.in_runtime(|| {
            write_store(&pump, |store| {
                add_toast(
                    store,
                    ToastId::new(ToastSource::Host { name: "test" }, SUBJECT),
                    "hello",
                    ToastKind::Warn,
                    ToastOptions::default(),
                    0,
                );
            });
        });
        self.settle();
    }
}

fn mounted() -> Mounted {
    PROBE_RENDERS.with(|count| count.set(0));
    let mut dom = VirtualDom::new(root);
    dom.rebuild_in_place();
    let mut mounted = Mounted {
        pump: BUILT.with(|built| built.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always builds a \
             pump; an empty slot means the render pass never happened",
        ),
        dom,
    };
    mounted.settle();
    mounted
}

/// THE REGRESSION. A host write that moves the store, through the helper the
/// notification cards use. Before the helper ended at the pump, the store's
/// counter moved and the subscribed probe stayed on the frame it first drew: a
/// dismissal, a hold, an undo or a raised agent notice changed the store and
/// the dock kept painting the card.
#[test]
fn a_store_write_reaches_a_subscribed_reader() {
    let mut mounted = mounted();
    let before = mounted.renders();
    assert!(before > 0, "the probe renders on mount");

    mounted.raise_toast();

    let live = mounted.pump.core().borrow().store().toasts.len();
    assert_eq!(
        live, 1,
        "the write itself must reach the store the next render reads"
    );
    assert!(
        mounted.renders() > before,
        "a store write that moves the store must repaint what `use_store` \
         subscribed, or the reader keeps the card they dismissed"
    );
}

/// WHAT THE COMPARE BUYS. A write that changes nothing must not repaint: the
/// pump's signal is written only when the store's own counter moved, so a host
/// that writes defensively costs no repaint at all.
#[test]
fn a_store_write_that_changes_nothing_does_not_repaint() {
    let mut mounted = mounted();
    let before = mounted.renders();

    let pump = mounted.pump.clone();
    mounted.dom.in_runtime(|| {
        write_store(&pump, |_store| {
            // A read framed as a write: the shape a defensive host write takes.
        });
    });
    mounted.settle();

    assert_eq!(
        mounted.renders(),
        before,
        "a write the store's counter did not move must not repaint anybody"
    );
}
