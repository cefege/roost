//! Main error enum for `rpi-ai` (coding-standards §5.1).
//!
//! Note: provider/stream failures on the streaming path are NOT delivered via
//! this enum — they are encoded as [`crate::types::StreamEvent::Error`] plus a
//! `stopReason` on the final assistant message (coding-standards §5.2, mirroring
//! the upstream "errors go into the stream" contract).

/// Error type for `rpi-ai` fallible operations (startup/persistence/class of
/// errors the caller must handle immediately).
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("http error: {0}")]
    Http(String),

    #[error("auth failed for provider {provider}: {reason}")]
    Auth { provider: String, reason: String },

    #[error("stream interrupted: {0}")]
    Stream(String),

    #[error("invalid model catalog: {0}")]
    ModelCatalog(String),

    #[error("credential store error: {0}")]
    CredentialStore(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
