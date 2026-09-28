//! What the terminal pane hands the DOM input controller: the mode reads it
//! consults per event, the two byte paths back to the session, and the
//! textarea's accessible name.
//!
//! Split out of `controller_dom` because this is the pane's half of the
//! contract and the controller is the browser's: they change for different
//! reasons, and the pane constructs the options long before any textarea
//! exists. wasm32 only — the paste path hands the controller a
//! `ClipboardEvent`. Ports the `TerminalInputOptions` props of v2
//! `apps/web/src/renderer/terminalInputController.ts`.

use web_sys::ClipboardEvent;

/// The pane's paste path: the text the controller confirmed, and the browser's
/// own event so the handler can read what the clipboard actually held.
pub type PasteHandler = Box<dyn Fn(&str, &ClipboardEvent)>;

/// What the pane hands the controller. The mode reads are called per event,
/// so a worker-reported DECCKM or DECSET 1004 change applies to the next key.
pub struct TerminalInputOptions {
    /// DECCKM as the worker reports it.
    pub cursor_keys_application: Box<dyn Fn() -> bool>,
    /// DECSET 1004 as the worker reports it: real focus and blur become PTY
    /// reports while the application asks for them.
    pub focus_events_enabled: Box<dyn Fn() -> bool>,
    /// Bytes for the session's input lane.
    pub on_data: Box<dyn Fn(&str)>,
    /// Clipboard admission belongs to the pane, so multiline confirmation,
    /// attachment extraction, bracketed framing and queue limits share one path.
    pub on_paste: PasteHandler,
    /// The textarea's accessible name; "Terminal input" when absent.
    pub aria_label: Option<String>,
    /// TV mode keeps directional navigation off the off-screen textarea.
    pub tv_mode_active: bool,
}

impl std::fmt::Debug for TerminalInputOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalInputOptions")
            .field("aria_label", &self.aria_label)
            .field("tv_mode_active", &self.tv_mode_active)
            .finish_non_exhaustive()
    }
}
