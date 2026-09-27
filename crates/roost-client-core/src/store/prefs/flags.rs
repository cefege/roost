//! The four boolean flags, and the four named writes that change them.
//!
//! They share one codec — `"1"` and `"0"`, read by `read_flag` with the default
//! as a parameter, because two of them default off and two default on — and
//! nothing else. Each write persists BEFORE the value is reported changed, so a
//! storage backend that fails cannot leave a preference that looks saved and is
//! not.
//!
//! Ported from `copyOnSelectPref.ts`, `keyboardResizePref.ts`,
//! `keytermBiasingPref.ts` and `mouseForwardPref.ts`; the deviation is the
//! unrecognised-value rule, which is in `prefs.rs`.

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::prefs::{
    COPY_ON_SELECT_KEY, KEYBOARD_RESIZE_KEY, KEYTERM_BIASING_KEY, MOUSE_FORWARD_KEY, write_flag,
};

/// Put a terminal selection on the clipboard when the drag ends.
///
/// Default off: it silently overwrites the system clipboard, which is surprising
/// if you did not ask for it.
pub fn set_copy_on_select(store: &mut Store, storage: &dyn KeyValueStore, on: bool) -> bool {
    if store.prefs.copy_on_select == on {
        return false;
    }
    store.prefs.copy_on_select = on;
    write_flag(storage, COPY_ON_SELECT_KEY, on);
    store.note_change();
    tracing::debug!(target: "store", on, "copy on select");
    true
}

/// Shrink the shell for the soft keyboard instead of pushing content up.
///
/// Default push. Resize is safe to offer in cell mode precisely because this
/// client never reflows history, so a height change cannot corrupt scrollback.
pub fn set_keyboard_resize(store: &mut Store, storage: &dyn KeyValueStore, on: bool) -> bool {
    if store.prefs.keyboard_resize == on {
        return false;
    }
    store.prefs.keyboard_resize = on;
    write_flag(storage, KEYBOARD_RESIZE_KEY, on);
    store.note_change();
    tracing::debug!(target: "store", on, "keyboard resize");
    true
}

/// Bias dictation toward the terminal's on-screen jargon.
///
/// Default on. Applies to the next recording.
pub fn set_keyterm_biasing(store: &mut Store, storage: &dyn KeyValueStore, on: bool) -> bool {
    if store.prefs.keyterm_biasing == on {
        return false;
    }
    store.prefs.keyterm_biasing = on;
    write_flag(storage, KEYTERM_BIASING_KEY, on);
    store.note_change();
    true
}

/// Let pointer and touch gestures reach the application.
///
/// The persisted ESCAPE HATCH, not the detector: forwarding is gated on what the
/// foreground program asked for, read off the core's mouse-tracking mode, so this
/// only overrides the case that mode cannot express — keeping native selection
/// inside an app that does request the mouse. The gesture handlers forward when
/// this is on AND the frame reports a nonzero tracking mode, and the pane's
/// `touch-action` reads the same predicate, so the two can never disagree about
/// who owns a drag.
pub fn set_mouse_forward(store: &mut Store, storage: &dyn KeyValueStore, on: bool) -> bool {
    if store.prefs.mouse_forward == on {
        return false;
    }
    store.prefs.mouse_forward = on;
    write_flag(storage, MOUSE_FORWARD_KEY, on);
    store.note_change();
    tracing::debug!(target: "store", on, "mouse forwarding");
    true
}

/// Flip the mouse-forwarding override, for the one caller that is a toggle rather
/// than a set (the mobile nav-mouse key).
pub fn toggle_mouse_forward(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    set_mouse_forward(store, storage, !store.prefs.mouse_forward)
}

/// Read a stored flag with the store's own default for it.
///
/// `mouseGesturesForwarded` is the one predicate the gesture handlers and the
/// pane's `touch-action` both call (`mouseForwardPref.ts:36-38`), so it is here
/// rather than at either of them.
pub fn mouse_gestures_forwarded(store: &Store, tracking_mode: u16) -> bool {
    store.prefs.mouse_forward && tracking_mode != 0
}
