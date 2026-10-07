//! The shell's composer slot: who holds it, and for how long.
//!
//! The compact shell reserves a row for the portaled composer and lifts the
//! notification dock above it from ONE published geometry, and both consumers
//! read it the same way — so every assertion here is on what the shell would
//! reserve, never on the bookkeeping behind it.
//!
//! THE REGRESSION this file exists for is the dock's own render. A dock holds
//! its claim by value in `use_hook`, which hands every render its own copy, and
//! a release hung on the value ran at the end of the render that took it: the
//! slot went back to "no composer mounted" while the dock was still on screen.
//! The compact shell then reserved only its resting row and the notification
//! dock's lift collapsed onto the composer. `a_dock_kept_in_a_hook_holds_the_slot`
//! reproduces exactly that shape — claim inside `use_hook`, component stays
//! mounted — and fails against a release hung on the value.
//!
//! THE VISIBILITY the claim follows is a drawer change, not a mount. A dock the
//! drawer covers keeps its component, so a claim taken once at mount leaves the
//! shell reserving for a composer the reader cannot see — and the element it
//! left behind measures `0`. `a_drawer_change_re_publishes_the_viewport_claim`
//! drives the real hook through that signal, and
//! `a_pane_dock_never_answers_to_the_shell_slot` pins the placement half of the
//! same rule.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_web::components::terminal_chrome::composer_claim::use_viewport_claim;
use roost_web::components::terminal_chrome::composer_gate;
use roost_web::components::terminal_chrome::composer_geometry::{
    ComposerClaim, ComposerSlot, published_geometry,
};
use roost_web::components::terminal_chrome::composer_placement::ComposerPlacement;

/// How many render-then-event rounds one change is given, so a queued re-render
/// has run before the assertion reads the slot.
const SETTLE_ROUNDS: usize = 4;

/// A claim dropped at the end of the test, so one test's slot state never
/// becomes the next test's.
struct ResetOnDrop(Option<ComposerClaim>);

impl Drop for ResetOnDrop {
    fn drop(&mut self) {
        self.0 = None;
    }
}

/// The dock, as the portaled composer builds it: the claim lives in `use_hook`
/// and the component stays mounted after the render that took it.
#[component]
fn HookedDock() -> Element {
    let slot = use_hook(|| Rc::new(ComposerSlot::new()));
    use_effect(move || slot.set_on_screen(true));
    rsx! { div { "hooked-dock" } }
}

thread_local! {
    /// The signal the root reads to show and hide the dock, handed back out of
    /// the render pass that created it.
    static SHOW: RefCell<Option<Signal<bool>>> = const { RefCell::new(None) };
}

/// The root `VirtualDom::new` wants: the visibility signal has to be created
/// inside the scope that owns it, so the root makes it and publishes a handle.
fn hooked_dock_root() -> Element {
    let show = use_hook(|| Signal::new(true));
    SHOW.with(|slot| *slot.borrow_mut() = Some(show));
    rsx! { if show() { HookedDock {} } }
}

/// A mounted dock that claims its slot from a hook, and the visibility signal
/// the caller drives it with.
struct Hooked {
    dom: VirtualDom,
    show: Signal<bool>,
}

impl Hooked {
    fn set_shown(&mut self, shown: bool) {
        self.show.set(shown);
        settle(&mut self.dom);
    }
}

fn mounted_hooked_dock() -> Hooked {
    let mut dom = VirtualDom::new(hooked_dock_root);
    dom.rebuild_in_place();
    settle(&mut dom);
    let show = SHOW.with(|slot| {
        slot.borrow().expect(
            "the root component runs during the first rebuild and always creates \
         its signal; an empty slot means the render pass never happened",
        )
    });
    Hooked { dom, show }
}

fn settle(dom: &mut VirtualDom) {
    for _ in 0..SETTLE_ROUNDS {
        dom.render_immediate(&mut NoOpMutations);
        dom.process_events();
    }
}

thread_local! {
    /// The drawer flag the claim harness drives, handed back out of the render
    /// pass that created it.
    static DRAWER_OPEN: RefCell<Option<Signal<bool>>> = const { RefCell::new(None) };
}

/// The dock, as `composer.rs` builds it: the visibility answer the body read
/// this render, handed to the claim hook that holds the slot.
#[component]
fn ClaimedDock(placement: ComposerPlacement, drawer_open: Signal<bool>) -> Element {
    let on_screen = composer_gate::dock_on_screen(placement, drawer_open());
    use_viewport_claim(placement, on_screen);
    rsx! { div { "claimed-dock" } }
}

/// One placement's root `VirtualDom::new` wants: the drawer signal has to be
/// created inside the scope that owns it, so the root makes it and publishes a
/// handle.
fn claimed_dock_root(placement: ComposerPlacement) -> Element {
    let drawer_open = use_hook(|| Signal::new(false));
    DRAWER_OPEN.with(|open| *open.borrow_mut() = Some(drawer_open));
    rsx! { ClaimedDock { placement, drawer_open } }
}

fn viewport_dock_root() -> Element {
    claimed_dock_root(ComposerPlacement::Viewport)
}

fn pane_dock_root() -> Element {
    claimed_dock_root(ComposerPlacement::Pane)
}

/// A mounted dock and the drawer signal the caller opens and closes it with.
struct Claimed {
    dom: VirtualDom,
    drawer_open: Signal<bool>,
}

impl Claimed {
    fn set_drawer_open(&mut self, open: bool) {
        self.drawer_open.set(open);
        settle(&mut self.dom);
    }
}

fn mounted_claimed_dock(root: fn() -> Element) -> Claimed {
    let mut dom = VirtualDom::new(root);
    dom.rebuild_in_place();
    settle(&mut dom);
    let drawer_open = DRAWER_OPEN.with(|open| {
        open.borrow().expect(
            "the root component runs during the first rebuild and always creates \
         its signal; an empty slot means the render pass never happened",
        )
    });
    Claimed { dom, drawer_open }
}

/// The drawer change has to RE-PUBLISH the claim, not merely un-render the dock.
///
/// The claim is taken from a reactive dependency rather than once at mount: a
/// mount-only claim keeps the shell reserving for a composer the drawer is
/// covering, and the dock it leaves detached measures `0` — the one height the
/// shell must never reserve from.
#[test]
fn a_drawer_change_re_publishes_the_viewport_claim() {
    let mut claimed = mounted_claimed_dock(viewport_dock_root);
    assert!(
        published_geometry().active,
        "a visible viewport dock holds the shell's composer slot"
    );

    claimed.set_drawer_open(true);
    assert!(
        !published_geometry().active,
        "the drawer covers the dock, so it holds nothing"
    );

    claimed.set_drawer_open(false);
    assert!(
        published_geometry().active,
        "the dock is uncovered and takes the slot again"
    );
}

/// The slot is the shell's reserve for a FIXED surface over the terminal. The
/// pane dock lives inside the pane, in a flow the pane already gives up rows
/// for, so a claim from one would make the shell reserve for a composer twice.
#[test]
fn a_pane_dock_never_answers_to_the_shell_slot() {
    let _pane = mounted_claimed_dock(pane_dock_root);
    assert!(
        !published_geometry().active,
        "only the viewport placement holds the shell's composer slot"
    );
}

/// A dock whose claim is taken inside `use_hook` still holds the slot once the
/// render that took it is over.
///
/// Without a reference-counted release the copy `use_hook` hands the render dies
/// with the render, the slot reports "no composer mounted", and the shell
/// reserves only its resting row — which is the whole of the notification dock
/// landing on top of the composer instead of above it.
#[test]
fn a_dock_kept_in_a_hook_holds_the_slot() {
    let _hooked = mounted_hooked_dock();
    assert!(
        published_geometry().active,
        "a mounted dock must leave the shell reserving for a composer"
    );
}

/// The dock leaving is what hands the slot back: the shell stops reserving for a
/// composer only once there is none on screen.
#[test]
fn a_dock_that_unmounts_releases_the_slot() {
    let mut hooked = mounted_hooked_dock();
    assert!(
        published_geometry().active,
        "held while the dock is mounted"
    );
    hooked.set_shown(false);
    assert!(
        !published_geometry().active,
        "the slot must go back when the dock unmounts"
    );
}

/// A dock the drawer covers gives the slot back even though the component stays
/// mounted — the drawer covers the surface, it does not unmount it — and takes
/// it again on the way out, from the same instance.
#[test]
fn a_dock_that_leaves_and_comes_back_keeps_its_hold() {
    let mut hooked = mounted_hooked_dock();
    let slot = Rc::new(ComposerSlot::new());
    hooked.set_shown(false);
    slot.set_on_screen(false);
    assert!(
        !published_geometry().active,
        "a dock that is not on screen holds nothing"
    );
    slot.set_on_screen(true);
    assert!(
        published_geometry().active,
        "a dock the drawer uncovers must hold the slot again"
    );
    // And the hold is real: the slot names this dock, and a measurement under
    // that identity lands.
    slot.set_on_screen(false);
    assert!(
        !published_geometry().active,
        "and hands it back when it leaves"
    );
}

#[test]
fn a_claim_marks_the_slot_active_before_any_measurement() {
    let _claim = ResetOnDrop(Some(ComposerClaim::claim()));
    assert!(published_geometry().active);
}

#[test]
fn dropping_the_owner_releases_the_slot_and_its_measured_height() {
    let mut claim = ResetOnDrop(Some(ComposerClaim::claim()));
    claim.0.as_ref().expect("held").publish(120.0);
    assert_eq!(published_geometry().height_px, 120.0);
    claim.0 = None;
    let geometry = published_geometry();
    assert!(!geometry.active);
    assert_eq!(geometry.height_px, 0.0);
}

#[test]
fn a_replacement_that_mounted_first_is_not_cleared_by_the_instance_it_replaced() {
    let replaced = ComposerClaim::claim();
    replaced.publish(96.0);
    let replacement = ComposerClaim::claim();
    replacement.publish(140.0);
    drop(replaced);
    let geometry = published_geometry();
    assert!(geometry.active, "the replacement still owns the slot");
    assert_eq!(geometry.height_px, 140.0);
    drop(replacement);
}

#[test]
fn a_disposed_dock_cannot_publish_a_height_into_a_slot_it_lost() {
    let first = ComposerClaim::claim();
    let second = ComposerClaim::claim();
    first.publish(999.0);
    assert_eq!(published_geometry().height_px, 0.0);
    drop(first);
    drop(second);
}

#[test]
fn releasing_the_owner_hands_the_slot_to_a_dock_that_is_still_mounted() {
    let viewport = ComposerClaim::claim();
    viewport.publish(72.0);
    let drawer = ComposerClaim::claim();
    drop(drawer);
    let geometry = published_geometry();
    assert!(
        geometry.active,
        "a mounted composer must not leave the shell reserving the inactive lift"
    );
    assert_eq!(geometry.height_px, 72.0);
    viewport.publish(80.0);
    assert_eq!(published_geometry().height_px, 80.0);
    drop(viewport);
    assert!(!published_geometry().active);
}

/// The portaled dock is a fixed surface the drawer covers, so it leaves with it;
/// the pane dock lives inside the pane the drawer covers wholesale.
#[test]
fn the_drawer_covers_the_portaled_dock_and_not_the_pane_dock() {
    assert!(composer_gate::dock_on_screen(
        ComposerPlacement::Viewport,
        false
    ));
    assert!(!composer_gate::dock_on_screen(
        ComposerPlacement::Viewport,
        true
    ));
    assert!(composer_gate::dock_on_screen(ComposerPlacement::Pane, true));
}
