//! Main error enum for `rpi-agent` (coding-standards §5.1).
//!
//! Streaming/provider failures are not delivered through this enum — they are
//! encoded as stream/agent events with a `stopReason` (coding-standards §5.2).
//! This enum is for failures the caller must handle immediately (session
//! persistence, invalid configuration, harness capability errors).

/// Error type for `rpi-agent` fallible operations.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("tool execution failed: {0}")]
    Tool(String),

    #[error("agent loop failure: {0}")]
    Loop(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("harness error: {0}")]
    Harness(String),

    /// Error whose text must match an upstream `Error` message verbatim
    /// (mutex guards, `continue` preconditions, tool-thrown failures). Use
    /// this variant whenever the message text is part of the parity contract;
    /// the other variants add a prefix.
    #[error("{0}")]
    Message(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
