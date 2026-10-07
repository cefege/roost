//! Whether the portaled composer dock is on screen, and the store read that
//! decides it.
//!
//! The pane decides whether to MOUNT a dock from a snapshot of the drawer flag
//! taken at its own last render, and a snapshot cannot un-render anything: the
//! reader who opens the drawer moves the store, the pane does not re-render for
//! it, and the composer the drawer has just covered stays on screen with its
//! field still in the tab order. So the dock asks for itself, on the store's
//! revision, and leaves when the answer is that it should.

use dioxus::prelude::*;

use super::composer_placement::ComposerPlacement;

/// What the dock reads off the store: the flag alone, so an unrelated revision
/// does not re-render a dock the reader is typing into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DrawerGate {
    open: bool,
}

/// Whether a composer at `placement` is on screen.
///
/// The pane placement lives INSIDE the pane, which the drawer covers wholesale,
/// so it is never one of these; only the portaled dock is a fixed surface that
/// outlives the drawer and has to leave with it.
#[must_use]
pub fn dock_on_screen(placement: ComposerPlacement, drawer_open: bool) -> bool {
    placement == ComposerPlacement::Pane || !drawer_open
}

/// Subscribe the calling component to the mobile drawer.
///
/// The subscription is the whole point: a plain read of the same flag answers
/// the same question on the render that happens to ask it, and nothing else.
#[must_use]
pub fn use_drawer_open() -> bool {
    let pump = crate::pump::use_store();
    let revision = pump.revision();
    let memo_pump = pump.clone();
    let gate = use_memo(use_reactive((&memo_pump,), move |(pump,)| {
        let _revision = revision.read();
        DrawerGate {
            open: pump.core().borrow().store().ui.sidebar_open,
        }
    }));
    gate().open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_portaled_dock_leaves_with_the_drawer_that_covers_it() {
        assert!(dock_on_screen(ComposerPlacement::Viewport, false));
        assert!(!dock_on_screen(ComposerPlacement::Viewport, true));
    }

    #[test]
    fn the_pane_dock_is_not_a_fixed_surface_the_drawer_can_leave_behind() {
        assert!(dock_on_screen(ComposerPlacement::Pane, true));
    }
}
