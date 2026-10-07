//! Per-device terminal ligature shaping preference.
//!
//! The value changes only text shaping, never the terminal's cell geometry.
//! The browser persists this independent of the coordinator account.

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::prefs::TERMINAL_LIGATURES_KEY;

/// Persist the terminal shaping choice.
pub fn set_terminal_ligatures(store: &mut Store, storage: &dyn KeyValueStore, on: bool) -> bool {
    if store.prefs.terminal_ligatures == on {
        return false;
    }
    store.prefs.terminal_ligatures = on;
    storage.set(TERMINAL_LIGATURES_KEY, if on { "1" } else { "0" });
    store.note_change();
    tracing::debug!(target: "store", on, "terminal ligature shaping changed");
    true
}
