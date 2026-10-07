//! Clipboard entry points exposed by `PaneMount`.
//! They keep clipboard plumbing separate from the renderer lifecycle and
//! delegate to the existing clipboard and paste-guard implementations.

use super::{PaneMount, clipboard, input};

impl PaneMount {
    /// Copy the current terminal selection through clipboard history.
    pub fn copy_selection(&self) {
        clipboard::copy_selection(&self.shared);
    }

    /// Read the browser clipboard and route it through the paste guard.
    pub fn paste_from_clipboard(&self) {
        input::paste_from_clipboard(&self.shared);
    }
}
