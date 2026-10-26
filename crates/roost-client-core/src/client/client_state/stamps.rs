//! The sidecar that keeps each synced key's stamp across reloads, so a remote
//! merge can still compare against it. The stamps are one JSON object under a
//! single local key. A corrupt sidecar loads as empty rather than blocking boot.

use std::collections::BTreeMap;

use crate::platform::KeyValueStore;

/// The local sidecar holding each synced key's stamp as one JSON object.
const META_STORAGE_KEY: &str = "roost.clientState.meta.v1";

pub(super) fn load_stamps(local: &dyn KeyValueStore) -> BTreeMap<String, i64> {
    let Some(text) = local.get(META_STORAGE_KEY) else {
        return BTreeMap::new();
    };
    match serde_json::from_str(&text) {
        Ok(stamps) => stamps,
        Err(error) => {
            tracing::warn!(
                target: "client_state",
                error = %error,
                "client state stamps unreadable; starting with none"
            );
            BTreeMap::new()
        }
    }
}

pub(super) fn save_stamps(local: &dyn KeyValueStore, stamps: &BTreeMap<String, i64>) {
    match serde_json::to_string(stamps) {
        Ok(json) => local.set(META_STORAGE_KEY, &json),
        Err(error) => tracing::warn!(
            target: "client_state",
            error = %error,
            "client state stamps not persisted"
        ),
    }
}
