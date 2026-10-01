//! Whether the worker stores an attachment under a short path, and the switch
//! that changes it.
//!
//! Read from the storage key it has always used rather than re-declared in the
//! store, because a preference written by the v2 build has to mean the same
//! thing to this one: a user who chose short paths does not silently get full
//! ones after the port, and the difference is visible in their scrollback.
//! Ports `getShortPathPref` / `setShortPathPref` in
//! `apps/web/src/lib/attachments.ts`.

use roost_client_core::KeyValueStore;

/// The storage key, and the one value that turns it on.
pub const SHORT_PATH_STORAGE_KEY: &str = "roost.useShortAttachPaths";

/// Whether attachments should be stored under a short path.
///
/// A private-mode tab throws on the read, and a preference that cannot be read
/// is a preference that was never set: the answer is the full path, which is
/// always correct and merely longer.
#[must_use]
pub fn short_path_preference() -> bool {
    storage().get(SHORT_PATH_STORAGE_KEY).as_deref() == Some("1")
}

/// Turn the preference on or off, returning whether it changed.
pub fn set_short_path_preference(on: bool) -> bool {
    let value = if on { "1" } else { "0" };
    storage().set(SHORT_PATH_STORAGE_KEY, value);
    true
}

/// The document's local storage. A window with no `localStorage` — a worker, a
/// private-mode tab that refused — gets the store's own volatile stand-in, so a
/// preference that cannot be persisted is still readable for this page rather
/// than throwing on every access.
fn storage() -> Box<dyn KeyValueStore> {
    Box::new(crate::platform::LocalStorageKeyValueStore::new())
}
