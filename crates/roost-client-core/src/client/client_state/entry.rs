//! The plain form of one synced browser key: the value as the store reads it,
//! and the epoch stamp that orders it against the same key on another device.

/// One account-scoped key as it crosses the coordinator boundary. An empty
/// `value` means the key is absent locally; a removal syncs as that empty value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStateEntry {
    pub key: String,
    pub value: String,
    pub updated_ms: i64,
}
