//! Preferences: the per-device settings that outlive a reload.
//!
//! One struct, loaded once at boot and written by the same functions that change
//! a value, so a stored preference and its in-memory value cannot disagree. Each
//! preference keeps its own codec in a module beside this one, because they are
//! genuinely different shapes: four flags, one JSON object, one enum, one
//! bounded number. What they share is the FALLBACK rule, and it lives here:
//!
//! **A value this build does not recognise is a value it did not write, and it
//! is ignored.** Absent, unparseable, and out-of-range all resolve to the
//! default, and loading never fails — a corrupt preference is a preference the
//! user has not chosen, not a boot that does not happen. That is the whole
//! contract of `load_prefs`: total, quiet, and one `revision` at the end.
//!
//! Ported from `apps/web/src/store/prefs/*` (7 files, 326 lines) and
//! `uiStore.ts`'s four persisted fields.

pub mod flags;
pub mod notify;
pub mod predict;
pub mod terminal_bell;
pub mod terminal_font;

pub use flags::{set_copy_on_select, set_keyboard_resize, set_keyterm_biasing, set_mouse_forward};
pub use notify::{NotifyPrefs, set_notify_pref};
pub use predict::{PredictMode, set_predict_mode};
pub use terminal_bell::{TerminalBell, set_terminal_bell};
pub use terminal_font::{set_term_font_px, step_term_font_px};

use self::terminal_font::{TERM_FONT_MAX_PX, TERM_FONT_MIN_PX, TERMINAL_FONT_DEFAULT_PX};
use crate::platform::KeyValueStore;
use crate::store::Store;

/// The stored key of a boolean flag written as `"1"` or `"0"`.
pub const COPY_ON_SELECT_KEY: &str = "roost.copyOnSelect";
/// See [`COPY_ON_SELECT_KEY`].
pub const KEYBOARD_RESIZE_KEY: &str = "roost.keyboardResize";
/// See [`COPY_ON_SELECT_KEY`].
pub const KEYTERM_BIASING_KEY: &str = "roost.keytermBiasing";
/// See [`COPY_ON_SELECT_KEY`].
pub const MOUSE_FORWARD_KEY: &str = "roostMouseForward";
/// See [`COPY_ON_SELECT_KEY`].
pub const NOTIFY_PREFS_KEY: &str = "roost.notifications.prefs.v2";
/// See [`COPY_ON_SELECT_KEY`].
pub const PREDICT_MODE_KEY: &str = "roostPredict";
/// See [`COPY_ON_SELECT_KEY`].
pub const TERM_FONT_PX_KEY: &str = "roost.termFontSize";

/// Every per-device preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefs {
    /// Put a terminal selection on the clipboard the moment the drag ends.
    ///
    /// Off by default: it silently overwrites the system clipboard, which
    /// surprises anyone who did not ask for it.
    pub copy_on_select: bool,
    /// Shrink the shell for the soft keyboard instead of pushing the content up.
    ///
    /// Push is the default because it is the calm one: the grid size never
    /// changes, so nothing recomputes while the keyboard slides in. Resize buys
    /// visible rows and costs a PTY resize on every frame of the ramp.
    pub keyboard_resize: bool,
    /// Bias dictation toward the terminal's on-screen jargon. On by default: it
    /// is the feature, and the toggle exists to A/B against plain transcription.
    pub keyterm_biasing: bool,
    /// Let pointer and touch gestures reach the application. On by default,
    /// because the gate is precise: an app that never asked for tracking never
    /// receives events either way, so the cost of an opt-in is mouse-aware TUIs
    /// losing their mouse.
    pub mouse_forward: bool,
    /// Per-browser notification preferences.
    pub notify: NotifyPrefs,
    /// How much speculative echo the terminal paints.
    pub predict: PredictMode,
    /// The terminal's font size in pixels, bounded on both sides.
    pub term_font_px: u32,
    /// What a terminal BEL does on this device. A flash by default: sound is
    /// an interruption the operator opts into.
    pub terminal_bell: TerminalBell,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            copy_on_select: false,
            keyboard_resize: false,
            keyterm_biasing: true,
            mouse_forward: true,
            notify: NotifyPrefs::default(),
            predict: PredictMode::Adaptive,
            term_font_px: TERMINAL_FONT_DEFAULT_PX,
            terminal_bell: TerminalBell::Visual,
        }
    }
}

impl Prefs {
    /// The defaults, before storage is read.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The values a device supplies rather than chooses.
///
/// One field today. A television three metres from the sofa wants a 20 px cell,
/// and that is a fact about the device the user is holding, not a preference
/// they set — so the HOST decides it and the store records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefDefaults {
    /// The font size a device gets before the user has chosen one.
    pub term_font_px: u32,
}

impl Default for PrefDefaults {
    fn default() -> Self {
        Self {
            term_font_px: TERMINAL_FONT_DEFAULT_PX,
        }
    }
}

/// Read every preference, falling back to the default for anything storage
/// cannot answer.
///
/// Returns whether anything changed, and bumps `revision` ONCE when it did: a
/// boot-time load is one repaint, not one per preference. Total by construction —
/// there is no error path, and no way for a corrupt value to stop a client from
/// starting.
pub fn load_prefs(store: &mut Store, storage: &dyn KeyValueStore, defaults: &PrefDefaults) -> bool {
    let next = Prefs {
        copy_on_select: read_flag(storage, COPY_ON_SELECT_KEY, false),
        keyboard_resize: read_flag(storage, KEYBOARD_RESIZE_KEY, false),
        // Absent means ON for these two, which is why the default is a parameter
        // of the read rather than a property of the codec.
        keyterm_biasing: read_flag(storage, KEYTERM_BIASING_KEY, true),
        mouse_forward: read_flag(storage, MOUSE_FORWARD_KEY, true),
        notify: notify::parse(storage.get(NOTIFY_PREFS_KEY).as_deref()),
        predict: predict::parse(storage.get(PREDICT_MODE_KEY).as_deref()),
        term_font_px: read_term_font_px(storage, defaults.term_font_px),
        terminal_bell: TerminalBell::parse(
            storage.get(terminal_bell::TERMINAL_BELL_KEY).as_deref(),
        ),
    };
    if next == store.prefs {
        return false;
    }
    store.prefs = next;
    store.note_change();
    tracing::debug!(target: "store", "preferences loaded");
    true
}

/// Forget every stored preference, at a credential boundary, and return the
/// in-memory ones to their defaults.
///
/// Notification preferences are per ACCOUNT in v2 — they say which machine the
/// user wants to be interrupted on — so they go with the credential rather than
/// with the device. The other four are about how this browser behaves and are
/// kept, which is why this is not "clear the lot".
pub fn clear_account_scoped_prefs(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    if store.prefs.notify == NotifyPrefs::default() {
        return false;
    }
    storage.remove(NOTIFY_PREFS_KEY);
    store.prefs.notify = NotifyPrefs::default();
    store.note_change();
    true
}

/// Read a flag written as `"1"` or `"0"`.
///
/// `default` is a parameter because the four flags do not agree: two default off
/// and two default on, and a codec that guessed would flip a preference the user
/// never touched. A stored value that is neither `"1"` nor `"0"` falls back to
/// `default` rather than being coerced — v2's `!== "0"` read turned a corrupt
/// value into `true`, which is a silent preference change dressed as a read.
pub(crate) fn read_flag(storage: &dyn KeyValueStore, key: &str, default: bool) -> bool {
    match storage.get(key).as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => default,
    }
}

/// Write a flag as `"1"` or `"0"`.
pub(crate) fn write_flag(storage: &dyn KeyValueStore, key: &str, on: bool) {
    storage.set(key, if on { "1" } else { "0" });
}

/// Read the terminal font size, bounded at BOTH ends and defaulted when it is
/// not a number at all.
///
/// The bound is derived once, from one pair of constants, and applied here and in
/// the setter. A bound applied on only one side is how a pane opens at 400 px:
/// below [`TERM_FONT_MIN_PX`] the cell stops being legible, and above
/// [`TERM_FONT_MAX_PX`] a normal pane holds so few columns that most TUIs
/// letterbox into uselessness.
fn read_term_font_px(storage: &dyn KeyValueStore, default_px: u32) -> u32 {
    match storage.get(TERM_FONT_PX_KEY) {
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(px) if px > 0 => px.clamp(TERM_FONT_MIN_PX, TERM_FONT_MAX_PX),
            _ => default_px.clamp(TERM_FONT_MIN_PX, TERM_FONT_MAX_PX),
        },
        None => default_px.clamp(TERM_FONT_MIN_PX, TERM_FONT_MAX_PX),
    }
}
