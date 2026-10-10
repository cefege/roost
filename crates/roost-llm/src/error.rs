//! The one error type every public async API in roost-llm returns. A variant
//! per failure the caller acts on differently: rotate on `RateLimited`,
//! disable the credential on `Auth`, stop on `Cancelled`.

/// A provider, network or credential failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    #[error("provider returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("rate limited (retry after {retry_after_ms:?} ms, reset at {reset_at_ms:?})")]
    RateLimited {
        retry_after_ms: Option<u64>,
        reset_at_ms: Option<i64>,
    },
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("could not decode provider response: {0}")]
    Decode(String),
    #[error("cancelled")]
    Cancelled,
    #[error("network error: {0}")]
    Network(String),
    #[error("no credential for provider {provider}")]
    NoCredential { provider: String },
}

impl LlmError {
    /// Whether a retry of the same request on the same credential may succeed:
    /// server errors, overload and transport failures.
    pub fn is_transient(&self) -> bool {
        match self {
            LlmError::Http { status, .. } => *status >= 500 || *status == 408 || *status == 529,
            LlmError::Network(_) => true,
            _ => false,
        }
    }
}

impl From<reqwest::Error> for LlmError {
    fn from(error: reqwest::Error) -> Self {
        LlmError::Network(error.to_string())
    }
}
