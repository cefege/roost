//! The account-scoped browser state that follows the operator across devices.
//!
//! One allowlist and one set of limits, shared by the coordinator (which
//! enforces them on every write) and the browser (which syncs only these keys).
//! A key outside the list is device-local by definition: the device key,
//! endpoint selection, pairing credentials and per-tab identity never leave
//! the browser that owns them.

use crate::error::{ProtocolError, ProtocolResult};

/// Exact keys that sync across devices.
pub const SYNCED_EXACT_KEYS: &[&str] = &[
    "roost.paneLayout.v1",
    "roost.lastSessionByFolder",
    "roost.lastTerminalPath",
    "roost.lastAgentConversation",
    "roost.sidebar.recent",
    "roost.composerDrafts.v1",
];

/// Key prefixes that sync across devices.
pub const SYNCED_KEY_PREFIXES: &[&str] = &["roost.lastWorkspaceId.", "roost.agent.draft."];

/// Longest key the server stores.
pub const MAX_KEY_BYTES: usize = 256;
/// Largest value the server stores for one key.
pub const MAX_VALUE_BYTES: usize = 256 * 1024;
/// Most entries one list or set call may carry.
pub const MAX_BATCH_ENTRIES: usize = 64;
/// Most keys one account may hold. A new key past this is refused; an
/// existing key may always be updated.
pub const MAX_KEYS_PER_ACCOUNT: usize = 1024;

/// Whether `key` is one the coordinator stores and the browser syncs.
pub fn is_synced_key(key: &str) -> bool {
    SYNCED_EXACT_KEYS.contains(&key)
        || SYNCED_KEY_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix) && key.len() > prefix.len())
}

/// Reject a key the allowlist does not cover, or one that is too long.
pub fn validate_key(field: &str, key: &str) -> ProtocolResult<()> {
    if key.len() > MAX_KEY_BYTES {
        return Err(ProtocolError::new(field, "key is too long"));
    }
    if !is_synced_key(key) {
        return Err(ProtocolError::new(
            field,
            "key is not a synced client-state key",
        ));
    }
    Ok(())
}

/// Reject a value over the size cap.
pub fn validate_value(field: &str, value: &str) -> ProtocolResult<()> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(ProtocolError::new(field, "value exceeds the size cap"));
    }
    Ok(())
}

/// Reject a batch over the entry cap.
pub fn validate_batch_len(field: &str, entries: usize) -> ProtocolResult<()> {
    if entries > MAX_BATCH_ENTRIES {
        return Err(ProtocolError::new(field, "batch exceeds the entry cap"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_admits_only_listed_keys_and_prefixes() {
        assert!(is_synced_key("roost.paneLayout.v1"));
        assert!(is_synced_key("roost.lastWorkspaceId.fp-1"));
        assert!(is_synced_key("roost.agent.draft.conv-1"));
        assert!(!is_synced_key("roost.lastWorkspaceId."));
        assert!(!is_synced_key("roost.agent.draft."));
        assert!(!is_synced_key("roost.deviceKey"));
        assert!(!is_synced_key("roost.coordinatorUrl"));
    }

    #[test]
    fn caps_are_enforced_at_the_boundary() {
        assert!(validate_key("k", &"x".repeat(MAX_KEY_BYTES)).is_err());
        assert!(validate_value("v", &"x".repeat(MAX_VALUE_BYTES)).is_ok());
        assert!(validate_value("v", &"x".repeat(MAX_VALUE_BYTES + 1)).is_err());
        assert!(validate_batch_len("b", MAX_BATCH_ENTRIES).is_ok());
        assert!(validate_batch_len("b", MAX_BATCH_ENTRIES + 1).is_err());
    }
}
