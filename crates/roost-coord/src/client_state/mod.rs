//! Account-scoped browser state that follows the operator across devices.
//!
//! The coordinator stores the allowlisted keys of `roost_protocol::client_state`
//! per account and merges concurrent writes by last-writer-wins on `updated_ms`.
//! `rpc` holds the two handlers, `store` the SQL. Values never enter logs.

mod rpc;
mod store;

pub use rpc::{handle_client_state_list, handle_client_state_set};

/// Why a client-state call was refused or failed. Each variant is a distinct
/// failure a browser can act on, so none collapses into another.
#[derive(Debug, thiserror::Error)]
pub enum ClientStateError {
    #[error("client-state key is longer than the {limit}-byte cap")]
    KeyTooLong { limit: usize },
    #[error("client-state value exceeds the {limit}-byte cap")]
    ValueTooLong { limit: usize },
    #[error("key is not a synced client-state key")]
    NotAllowlisted,
    #[error("batch exceeds the {limit}-entry cap")]
    BatchTooLarge { limit: usize },
    #[error("account already holds the {limit} client-state keys it may store")]
    KeyCapReached { limit: usize },
    #[error("client-state storage failed: {0}")]
    Database(#[from] sqlx::Error),
}

impl ClientStateError {
    /// A stable label for logs. The message can carry a limit; no variant carries a value.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::KeyTooLong { .. } => "key_too_long",
            Self::ValueTooLong { .. } => "value_too_long",
            Self::NotAllowlisted => "not_allowlisted",
            Self::BatchTooLarge { .. } => "batch_too_large",
            Self::KeyCapReached { .. } => "key_cap_reached",
            Self::Database(_) => "database",
        }
    }
}
