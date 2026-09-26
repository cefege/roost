//! `KeyValueStore` over `window.localStorage`.
//!
//! Owned by `platform`, called by the client core's `PersistWatermark` handling
//! and by the pairing tab id, and depends on nothing but the DOM storage API.
//!
//! Every operation is best-effort and none of them reports failure, because the
//! trait has no way to: a browser in private mode throws `QuotaExceededError` on
//! `setItem` and a Safari tab with cookies blocked throws on `localStorage`
//! access itself. v2 reached the same conclusion the same way — each preference
//! wrapped its own `try`/`catch` and updated its in-memory signal first
//! (`apps/web/src/store/sync-frame.ts`). The state machine stays correct when
//! the watermark does not persist: the next dial is a full hydration instead of a
//! backfill, which is slower and never wrong.

use std::cell::RefCell;

use roost_client_core::KeyValueStore;
use web_sys::{Storage, Window};

/// `localStorage`, or an in-memory stand-in where storage is unavailable.
///
/// The stand-in exists because a browser can refuse the property ACCESS itself,
/// not only a write, and a client that panicked in its constructor would take
/// the whole tab with it. Losing the watermark is a slower reconnect; losing the
/// page is not a trade this makes.
#[derive(Debug)]
pub struct LocalStorageKeyValueStore {
    storage: Option<Storage>,
    /// Only populated when `storage` is `None`.
    volatile: RefCell<Vec<(String, String)>>,
}

impl LocalStorageKeyValueStore {
    /// A store over this window's `localStorage`.
    pub fn new() -> Self {
        Self::for_window(web_sys::window())
    }

    /// A store over a named window's `localStorage`, or the stand-in.
    pub fn for_window(window: Option<Window>) -> Self {
        let storage = window
            .and_then(|window| window.local_storage().ok())
            .flatten();
        Self {
            storage,
            volatile: RefCell::new(Vec::new()),
        }
    }

    /// Whether real storage is behind this store.
    ///
    /// Diagnostics and the pairing pane: a browser that refused storage is a
    /// reconnect that re-hydrates every time, and the user should be able to see
    /// that rather than guess at it.
    pub fn is_persistent(&self) -> bool {
        self.storage.is_some()
    }
}

/// `sessionStorage`, on the same terms as `LocalStorageKeyValueStore`.
///
/// A separate type rather than a flag on the local one because the difference is
/// a security property, not a preference: the pairing credential captured from a
/// URL fragment is retained HERE and nowhere else, so closing the tab destroys
/// it. A store that could be pointed at either area would eventually be pointed
/// at the wrong one.
#[derive(Debug)]
pub struct SessionStorageKeyValueStore {
    inner: LocalStorageKeyValueStore,
}

impl SessionStorageKeyValueStore {
    /// A store over this window's `sessionStorage`.
    pub fn new() -> Self {
        let storage = web_sys::window()
            .and_then(|window| window.session_storage().ok())
            .flatten();
        Self {
            inner: LocalStorageKeyValueStore {
                storage,
                volatile: RefCell::new(Vec::new()),
            },
        }
    }

    /// Whether real session storage is behind this store.
    pub fn is_persistent(&self) -> bool {
        self.inner.is_persistent()
    }
}

impl Default for SessionStorageKeyValueStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyValueStore for SessionStorageKeyValueStore {
    fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }

    fn set(&self, key: &str, value: &str) {
        self.inner.set(key, value);
    }

    fn remove(&self, key: &str) {
        self.inner.remove(key);
    }
}

impl Default for LocalStorageKeyValueStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyValueStore for LocalStorageKeyValueStore {
    fn get(&self, key: &str) -> Option<String> {
        match &self.storage {
            Some(storage) => storage.get_item(key).ok().flatten(),
            None => self
                .volatile
                .borrow()
                .iter()
                .find(|(stored, _)| stored == key)
                .map(|(_, value)| value.clone()),
        }
    }

    fn set(&self, key: &str, value: &str) {
        match &self.storage {
            Some(storage) => {
                // A refused write is not an error the caller can act on and must
                // not be an error the caller has to remember to swallow.
                let _ = storage.set_item(key, value);
            }
            None => {
                let mut entries = self.volatile.borrow_mut();
                entries.retain(|(stored, _)| stored != key);
                entries.push((key.to_string(), value.to_string()));
            }
        }
    }

    fn remove(&self, key: &str) {
        match &self.storage {
            Some(storage) => {
                let _ = storage.remove_item(key);
            }
            None => self
                .volatile
                .borrow_mut()
                .retain(|(stored, _)| stored != key),
        }
    }
}
