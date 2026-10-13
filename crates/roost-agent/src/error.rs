//! The harness's error type. A variant per failure the coordinator maps to a
//! different RPC status: a missing conversation, a rejected argument, a busy
//! run, a storage failure, or a model/provider failure.

/// A harness failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error("conversation {0} not found")]
    NotFound(String),
    #[error("{0}")]
    InvalidArgument(String),
    #[error("{0}")]
    FailedPrecondition(String),
    #[error("agent storage failed: {0}")]
    Store(String),
    #[error("{0}")]
    Model(String),
    #[error("cancelled")]
    Cancelled,
    /// A compare-and-set write lost to a concurrent writer.
    #[error("conversation {0} was changed concurrently")]
    Conflict(String),
    /// The run lease this write carried was taken by another replica.
    #[error("conversation {0} run is owned by another coordinator")]
    Fenced(String),
}

impl From<roost_llm::LlmError> for AgentError {
    fn from(error: roost_llm::LlmError) -> Self {
        match error {
            roost_llm::LlmError::Cancelled => AgentError::Cancelled,
            other => AgentError::Model(other.to_string()),
        }
    }
}
