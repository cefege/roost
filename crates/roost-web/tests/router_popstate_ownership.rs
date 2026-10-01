//! The `popstate` listener belongs to the scope that installed it.
//!
//! `use_path_signal` wrote a `popstate` handler and called `Closure::forget`,
//! on the reasoning that a shell is mounted once for the life of the document.
//! Nothing enforced that: the signal is scope state, the access gate remounts
//! it (pair, unpair, authorized, unauthorized), and every remount left another
//! forgotten handler on `window` — bound to a signal whose scope was already
//! gone, so the next Back press wrote into a dropped signal and panicked in
//! `Signal::set`. The leak and the panic are the same defect.
//!
//! The fix is ownership: the registration is a value the scope holds, and the
//! scope's end takes it off `window`. The DOM half of that can only be exercised
//! in a browser, so what these pin is the half a native host can reach — that a
//! path signal never outlives the scope whose listener writes it, and that a
//! registration releases exactly once, for the owner that drops it.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_web::router_state::{PopstateListener, use_path_signal};

/// How many render-then-effect rounds a mount or an unmount is given, so the
/// scope teardown this asks about has run before the assertion reads the signal.
const SETTLE_ROUNDS: usize = 4;

thread_local! {
    /// Whether the path holder is mounted, owned by the root.
    static SHOWN: RefCell<Option<Signal<bool>>> = const { RefCell::new(None) };
    /// The path signal the holder was handed, published on its first render.
    static PATH: RefCell<Option<Signal<String>>> = const { RefCell::new(None) };
}

/// The component whose scope the listener belongs to.
#[component]
fn PathHolder() -> Element {
    let path = use_path_signal();
    PATH.with(|published| {
        if published.borrow().is_none() {
            *published.borrow_mut() = Some(path);
        }
    });
    rsx! { div { "holder" } }
}

/// The root `VirtualDom::new` wants: one child the test can mount and unmount.
fn root() -> Element {
    use_context_provider(|| {
        let shown = Signal::new(true);
        SHOWN.with(|slot| *slot.borrow_mut() = Some(shown));
        shown
    });
    let shown = use_context::<Signal<bool>>();
    rsx! {
        if shown() {
            PathHolder {}
        }
    }
}

/// Run the work a change queued, in the order the browser runs it.
fn settle(dom: &mut VirtualDom) {
    for _ in 0..SETTLE_ROUNDS {
        dom.render_immediate(&mut NoOpMutations);
        dom.process_events();
    }
}

/// A mounted shell, and the switch that unmounts the scope holding the signal.
struct Shell {
    dom: VirtualDom,
    shown: Signal<bool>,
    path: Signal<String>,
}

impl Shell {
    /// Unmount the scope, the way a gate that re-checks its access does.
    fn unmount(&mut self) {
        self.dom.in_runtime(|| self.shown.set(false));
        settle(&mut self.dom);
    }
}

fn mounted() -> Shell {
    let mut dom = VirtualDom::new(root);
    dom.rebuild_in_place();
    settle(&mut dom);
    Shell {
        shown: SHOWN
            .with(|slot| slot.borrow_mut().take())
            .expect("the root provides the switch during its first render"),
        path: PATH
            .with(|slot| slot.borrow_mut().take())
            .expect("the holder publishes its path signal during its first render"),
        dom,
    }
}

/// THE INVARIANT `forget` BROKE. The path signal is the value a `popstate`
/// handler writes, and it is scope state. If the scope takes it down, a listener
/// that outlived it is a callback on `window` aimed at a dropped signal; if the
/// signal outlived the scope instead, every remount would compound. Either way
/// the same mount is the one answer, and this pins which half dies.
#[test]
fn a_path_signal_does_not_outlive_the_scope_that_owns_it() {
    let mut shell = mounted();
    assert!(
        shell.path.try_read().is_ok(),
        "a mounted shell has a live path signal; a dropped one would make the rest of \
         this test pass for the wrong reason"
    );

    shell.unmount();

    assert!(
        shell.path.try_read().is_err(),
        "the signal a popstate handler writes is scope state and must be dropped with \
         the scope, or the listener the scope installed is left writing into nothing"
    );
}

/// THE REGISTRATION'S CONTRACT, which is what replaced `forget`. One owner, one
/// release, and nothing before the owner goes: a registration that released
/// early would leave `window` calling a callback whose `Closure` had already
/// been freed, which is the failure the `forget` was written to avoid.
#[test]
fn a_registration_releases_once_and_only_for_its_owner() {
    let released = Rc::new(Cell::new(0_u32));
    let survivor = Rc::new(Cell::new(0_u32));

    let owner = released.clone();
    let doomed = PopstateListener::new(move || owner.set(owner.get() + 1));
    let kept = survivor.clone();
    let live = PopstateListener::new(move || kept.set(kept.get() + 1));
    assert_eq!(
        (released.get(), survivor.get()),
        (0, 0),
        "live registrations have released nothing; both listeners are still on `window`"
    );

    drop(doomed);
    assert_eq!(
        released.get(),
        1,
        "dropping an owner takes its listener off `window` exactly once"
    );
    assert_eq!(
        survivor.get(),
        0,
        "one owner's release must not take the next registration's listener with it"
    );

    drop(live);
    assert_eq!(
        survivor.get(),
        1,
        "the surviving owner releases when it is dropped, so a remount replaces its \
         listener rather than stacking another beside it"
    );
}
