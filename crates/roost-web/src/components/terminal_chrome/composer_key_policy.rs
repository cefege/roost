//! Terminal input keys that alter the composer rather than the PTY draft.

use dioxus::prelude::KeyboardEvent;

/// Keep IME Return and soft-keyboard Return available to finish their input.
pub(super) fn enter_submits(event: &KeyboardEvent) -> bool {
    !event.is_composing() && !crate::components::deck::deck_dom::is_touch_device()
}
