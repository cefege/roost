//! The global keydown router: the command palette (⌘K), the help overlay
//! (Shift+?), terminal zoom (⌘=/⌘-/⌘0), settings (⌘,) and ↑/↓/⏎ over the
//! sidebar's flat cursor, with the terminal keeping every key it owns. Ports
//! `apps/web/src/lib/keyboardShortcuts.ts`; installed once by `App` (the
//! listener is `keyboard_shortcuts_dom`), and the overlay flags it owns are read
//! by the palette, help overlay and controller map.
//!
//! `keydown_action` is the whole decision; the listener only gathers
//! [`KeydownContext`] from the document and performs the answer.

use dioxus::prelude::*;

use crate::platform::browser_platform::{
    BrowserPlatform, PlatformShortcut, ShortcutKey, matches_platform_shortcut,
};

/// The open flags of the overlays this router toggles, provided once by `App`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShortcutOverlays {
    /// The command palette.
    pub palette: Signal<bool>,
    /// The keyboard help overlay.
    pub help: Signal<bool>,
    /// The controller button map (opened by a pad's Start, never by a key).
    pub controller_map: Signal<bool>,
}

impl ShortcutOverlays {
    /// Provide the flags in the calling (root) scope.
    pub fn provide() -> Self {
        use_context_provider(|| Self {
            palette: Signal::new(false),
            help: Signal::new(false),
            controller_map: Signal::new(false),
        })
    }
}

/// The overlay flags, from context.
pub fn use_shortcut_overlays() -> ShortcutOverlays {
    use_context::<ShortcutOverlays>()
}

/// Everything the decision reads besides the key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeydownContext {
    /// This browser's platform.
    pub platform: BrowserPlatform,
    /// A handler already consumed the event.
    pub default_prevented: bool,
    /// The command palette is open.
    pub palette_open: bool,
    /// The help overlay is open.
    pub help_open: bool,
    /// The controller map is open.
    pub controller_map_open: bool,
    /// A terminal deck (or pane) is on screen: the terminal owns ↑/↓/⏎.
    pub terminal_owns_keyboard: bool,
    /// The target is inside a terminal's input (`.wterm`).
    pub target_in_terminal_input: bool,
    /// Focus sits on `<body>`/`<html>` (a terminal that dropped focus).
    pub focus_on_body: bool,
    /// The target is an `<input>` or `<textarea>`.
    pub target_is_text_field: bool,
    /// The target is editable (text field or contenteditable).
    pub target_editable: bool,
    /// A TV remote or pad drives the arrows.
    pub directional_input_active: bool,
    /// The sidebar cursor highlights a row.
    pub cursor_row_selected: bool,
    /// The sidebar published rows the cursor can move over.
    pub has_cursor_targets: bool,
}

/// What a key press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeydownAction {
    /// Not ours: the event continues untouched.
    Ignore,
    /// Close the palette.
    ClosePalette,
    /// Open or close the palette.
    TogglePalette,
    /// Open or close the help overlay.
    ToggleHelp,
    /// Step the terminal font by this many pixels.
    StepTermFont(i32),
    /// Reset the terminal font.
    ResetTermFont,
    /// Navigate to the settings shell.
    OpenSettings,
    /// Open the highlighted sidebar row.
    ActivateCursor,
    /// Move the sidebar cursor by this many rows.
    MoveCursor(i32),
}

impl KeydownAction {
    /// Whether the event is consumed (`preventDefault`).
    pub const fn prevents_default(self) -> bool {
        !matches!(self, Self::Ignore)
    }
}

/// Decide what `key` does in `context`.
pub fn keydown_action(key: &ShortcutKey, context: &KeydownContext) -> KeydownAction {
    use KeydownAction as A;
    if context.default_prevented {
        return A::Ignore;
    }
    let platform = context.platform;
    let matches = |shortcut| matches_platform_shortcut(key, shortcut, platform);
    if context.palette_open && key.key == "Escape" {
        return A::ClosePalette;
    }
    // Punctuation and digit chords: safe before terminal focus recovery.
    if context.terminal_owns_keyboard {
        if matches(PlatformShortcut::TermFontIncrease) {
            return A::StepTermFont(1);
        }
        if matches(PlatformShortcut::TermFontDecrease) {
            return A::StepTermFont(-1);
        }
        if matches(PlatformShortcut::TermFontReset) {
            return A::ResetTermFont;
        }
    }
    if matches(PlatformShortcut::Settings) {
        return A::OpenSettings;
    }
    // Windows' shifted chord never collides with a PTY key, so it goes first.
    if platform == BrowserPlatform::Windows && matches(PlatformShortcut::CommandPalette) {
        return A::TogglePalette;
    }
    // The terminal encodes every non-Meta key it owns before anything here.
    if !key.meta
        && (context.target_in_terminal_input || (context.terminal_owns_keyboard && context.focus_on_body))
    {
        return A::Ignore;
    }
    if matches(PlatformShortcut::CommandPalette) {
        return A::TogglePalette;
    }
    if key.shift && key.key == "?" {
        return if context.target_is_text_field { A::Ignore } else { A::ToggleHelp };
    }
    if !matches!(key.key.as_str(), "ArrowUp" | "ArrowDown" | "Enter") {
        return A::Ignore;
    }
    let blocked = context.palette_open
        || context.help_open
        || context.controller_map_open
        || context.target_editable
        || context.terminal_owns_keyboard
        || key.meta
        || key.ctrl
        || key.alt
        || key.shift
        || context.directional_input_active;
    if blocked {
        return A::Ignore;
    }
    // ⏎ is a focused control's own activation until a row is highlighted; the
    // arrows are the document's native scroll until rows exist at all.
    match key.key.as_str() {
        "Enter" if context.cursor_row_selected => A::ActivateCursor,
        "ArrowDown" if context.has_cursor_targets => A::MoveCursor(1),
        "ArrowUp" if context.has_cursor_targets => A::MoveCursor(-1),
        _ => A::Ignore,
    }
}
