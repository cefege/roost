//! The terminal font size, bounded at both ends and rounded to whole pixels.
//!
//! Changing this changes the measured cell box, which changes cols and rows for a
//! fixed pane — so every pane re-measures and re-claims, which really is a PTY
//! resize round trip. It is unavoidable and correct, and it is why the value is
//! clamped BEFORE it is stored rather than after: a pane that measured 400 px
//! once has already reflowed.
//!
//! The bound is one pair of constants, applied by the loader, by the setter and by
//! the stepper. Both ends come from the same place, which is the only way a range
//! stays the same range in three places.

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::prefs::TERM_FONT_PX_KEY;

/// The size a device gets before the user has chosen one.
pub const TERMINAL_FONT_DEFAULT_PX: u32 = 14;
/// The size a television gets before the user has chosen one. A television sits
/// about three metres away, where a 14 px cell is unreadable.
pub const TERMINAL_FONT_TV_DEFAULT_PX: u32 = 20;
/// Below this the cell box stops being legible.
pub const TERM_FONT_MIN_PX: u32 = 9;
/// Above this a normal pane holds so few columns that most TUIs letterbox into
/// uselessness.
pub const TERM_FONT_MAX_PX: u32 = 28;

/// Clamp a size to the range, rounding to whole pixels.
///
/// One function, and every path into the store goes through it: a drag, a stepper
/// button, a stored value from a build that shipped a different bound.
pub fn clamp_term_font_px(px: u32) -> u32 {
    px.clamp(TERM_FONT_MIN_PX, TERM_FONT_MAX_PX)
}

/// Set the size, and persist.
pub fn set_term_font_px(store: &mut Store, storage: &dyn KeyValueStore, px: u32) -> bool {
    let next = clamp_term_font_px(px);
    if store.prefs.term_font_px == next {
        return false;
    }
    store.prefs.term_font_px = next;
    storage.set(TERM_FONT_PX_KEY, &next.to_string());
    store.note_change();
    tracing::debug!(target: "store", px = next, "terminal font size");
    true
}

/// Move the size by `delta` pixels, within the same bound.
pub fn step_term_font_px(store: &mut Store, storage: &dyn KeyValueStore, delta: i32) -> bool {
    // A negative delta must clamp at the same floor a direct set does, so the
    // stepper and the setter cannot disagree about where the range ends.
    let current = i32::try_from(store.prefs.term_font_px).unwrap_or(TERM_FONT_MIN_PX);
    let moved = current.saturating_add(delta);
    let bounded = u32::try_from(moved).unwrap_or(if moved < 0 { 0 } else { u32::MAX });
    set_term_font_px(store, storage, clamp_term_font_px(bounded))
}

/// Return the size to this device's default.
pub fn reset_term_font_px(store: &mut Store, storage: &dyn KeyValueStore, default_px: u32) -> bool {
    set_term_font_px(store, storage, default_px)
}
