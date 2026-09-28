//! The IndexedDB schema the browser device key lives in, and the upgrade plan
//! that reaches it. Owned by `platform::device_key`; read by its `indexed_db`
//! adapter, which only performs the steps planned here. Target-independent so
//! the migration is decided by native tests. Ported from
//! `apps/web/src/client/auth/web-key-storage.ts`: the SAME database, version,
//! store and slot, because a v2 profile's key must be the key this build loads.

pub use roost_client_core::client::auth::keystore::DEVICE_KEY_SLOT;

/// The database v2 created: one per origin, shared with every v2 tab.
pub const DATABASE_NAME: &str = "roost-auth";

/// The schema version. Opening at a lower number than a profile already holds
/// is a `VersionError`, so this is v2's number exactly.
pub const DATABASE_VERSION: u32 = 2;

/// The object store the key pair is written into, keyed out-of-line.
pub const KEY_STORE_NAME: &str = "keys";

/// The version-1 store of coordinator fingerprints, retired in version 2.
pub const LEGACY_TRUST_STORE_NAME: &str = "trust";

/// What one `upgradeneeded` must do to the stores it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaUpgrade {
    /// Create `keys`. False when it already exists: creating it again throws,
    /// and it holds the key a version-1 profile is still using.
    pub create_key_store: bool,
    /// Drop `trust`. Nothing reads it and it names coordinators by a
    /// fingerprint the pairing flow no longer trusts on that basis.
    pub delete_legacy_trust_store: bool,
}

/// The upgrade for a database that currently holds the named stores.
pub fn plan_schema_upgrade(has_key_store: bool, has_legacy_trust_store: bool) -> SchemaUpgrade {
    SchemaUpgrade {
        create_key_store: !has_key_store,
        delete_legacy_trust_store: has_legacy_trust_store,
    }
}

#[cfg(test)]
mod tests {
    use super::{SchemaUpgrade, plan_schema_upgrade};

    /// v2 `webKeyJwtCache.test.ts` "v1 key material survives trust-store
    /// removal": the version-1 `keys` store is kept, `trust` is dropped.
    #[test]
    fn a_version_one_database_keeps_its_keys_and_drops_trust() {
        assert_eq!(
            plan_schema_upgrade(true, true),
            SchemaUpgrade {
                create_key_store: false,
                delete_legacy_trust_store: true,
            }
        );
    }

    /// The same test's second half: a fresh profile gains `keys` and never
    /// tries to delete a `trust` store it does not have.
    #[test]
    fn a_fresh_database_gains_a_key_store_and_deletes_nothing() {
        assert_eq!(
            plan_schema_upgrade(false, false),
            SchemaUpgrade {
                create_key_store: true,
                delete_legacy_trust_store: false,
            }
        );
    }
}
