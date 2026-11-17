//! Origin-only access bookmarks, retained under the existing storage key.
//! The access surface owns updates; restoration sanitizes legacy URLs in place.
//! Browser authorization and transport selection never come from this list.

use roost_client_core::KeyValueStore;
use roost_client_core::client::access::enrollment::browser_origin;

const SAVED_COORDINATORS_KEY: &str = "roost.coords";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedCoordinator {
    pub name: String,
    pub url: String,
}

pub fn restore_saved(storage: &dyn KeyValueStore) -> Vec<SavedCoordinator> {
    let Some(raw) = storage.get(SAVED_COORDINATORS_KEY) else {
        return Vec::new();
    };
    let Ok(decoded) = serde_json::from_str::<Vec<SavedCoordinator>>(&raw) else {
        storage.remove(SAVED_COORDINATORS_KEY);
        return Vec::new();
    };
    let mut changed = false;
    let saved: Vec<_> = decoded
        .into_iter()
        .filter_map(|mut entry| {
            let Some(origin) = browser_origin(&entry.url) else {
                changed = true;
                return None;
            };
            changed |= origin != entry.url;
            entry.url = origin;
            Some(entry)
        })
        .collect();
    if changed {
        persist(storage, &saved);
    }
    saved
}

pub fn persist(storage: &dyn KeyValueStore, saved: &[SavedCoordinator]) {
    if let Ok(encoded) = serde_json::to_string(saved) {
        storage.set(SAVED_COORDINATORS_KEY, &encoded);
    }
}

#[cfg(test)]
mod tests {
    use super::{SAVED_COORDINATORS_KEY, restore_saved};
    use roost_client_core::{KeyValueStore, MemoryKeyValueStore};

    #[test]
    fn credential_bearing_legacy_bookmarks_are_removed_from_durable_storage() {
        let storage = MemoryKeyValueStore::new();
        storage.set(SAVED_COORDINATORS_KEY, r#"[{"name":"leaked","url":"https://home.test?pair=roost_bt_secret"},{"name":"home","url":"https://home.test"}]"#);
        let restored = restore_saved(&storage);
        assert_eq!(
            restored
                .iter()
                .map(|entry| entry.url.as_str())
                .collect::<Vec<_>>(),
            ["https://home.test"]
        );
        let persisted = storage.get(SAVED_COORDINATORS_KEY);
        assert!(persisted.as_deref().is_some_and(
            |value| value.contains("https://home.test") && !value.contains("roost_bt_secret")
        ));
    }
}
