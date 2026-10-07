//! `PaneMount`'s keyboard-and-text entry points: the key sheet's named keys,
//! composed and typed text, guarded pastes, and focus. A second inherent impl
//! of the struct in `pane_mount.rs`; each method forwards to `input`, which
//! owns the encoding. Called through `PaneHandle`.

use super::{PaneMount, input};

impl PaneMount {
    /// Send one named key through the textarea encoder (the key sheet).
    pub fn dispatch_key(&self, key: &str) {
        input::dispatch_named_key(&self.shared, key);
    }

    /// Send composed text, framed like a paste, optionally submitted.
    pub fn send_text(&self, text: &str, submit: bool) {
        input::send_text(&self.shared, text, submit);
    }

    /// Send text as typed bytes, never framed as a paste.
    pub fn send_raw_text(&self, text: &str) {
        input::send_bytes(&self.shared, text.as_bytes().to_vec(), false);
    }

    /// Type text as the keyboard would, spending a latched Ctrl on it.
    pub fn type_text(&self, text: &str) {
        input::on_controller_data(&self.shared, text);
    }

    /// Paste text through the multiline guard.
    pub fn paste_text(&self, text: &str) {
        input::paste_text(&self.shared, text);
    }

    /// Give the keyboard to the pane's textarea.
    pub fn force_focus(&self) {
        if let Some(controller) = self.shared.input.borrow().as_ref() {
            controller.force_focus();
        }
    }
}
