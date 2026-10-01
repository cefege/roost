//! The dock's hold on the shell's composer slot, and the visibility that
//! decides it.
//!
//! Two reads of one answer. The composer body reads the drawer to decide
//! whether to RENDER the dock at all — a composer under the drawer is a control
//! the reader cannot reach and cannot dismiss — and this hook re-asserts the
//! dock's CLAIM whenever that answer changes. The claim follows VISIBILITY
//! rather than the mount because the drawer covers the dock and uncovers it
//! without unmounting it: a claim that outlived the drawer left the shell sizing
//! the terminal against a composer nobody can see, and a covered dock's
//! detached element measures `0` — the one height the shell must never reserve
//! from.
//!
//! The drawer read STAYS in the body on purpose. `use_drawer_open` is a hook,
//! and a hook cannot run inside an effect, so the body subscribes the component
//! to the drawer and `use_reactive` carries that answer into the effect as a
//! reactive dependency. Without it the effect runs once at mount and the claim
//! is frozen at whatever the drawer was doing then.
//!
//! Ports `activeViewportToken` / `composerActive` from
//! `apps/web/src/components/terminal/TerminalComposeButton.tsx`, where Solid
//! unmounts the portaled dock under the drawer and disposing it drops the
//! composer owner. Here the dock is covered two ways — the pane stops mounting
//! it, and the composer itself renders nothing — so the claim follows the
//! visibility answer instead of the component's lifetime.

use std::rc::Rc;

use dioxus::prelude::*;

use super::composer::ComposerPlacement;
use super::composer_geometry::ComposerSlot;

/// The shell's composer slot for this dock, held for as long as it is on screen.
///
/// `None` for the pane placement: that dock lives inside the pane the shell
/// does not reserve rows for, so it never answers to the slot.
pub fn use_viewport_claim(
    placement: ComposerPlacement,
    on_screen: bool,
) -> Option<Rc<ComposerSlot>> {
    let slot = use_hook(|| {
        (placement == ComposerPlacement::Viewport).then(|| Rc::new(ComposerSlot::new()))
    });
    let claimed_slot = slot.clone();
    use_effect(use_reactive((&on_screen,), move |(on_screen,)| {
        if let Some(slot) = claimed_slot.as_ref() {
            slot.set_on_screen(on_screen);
        }
    }));
    slot
}
