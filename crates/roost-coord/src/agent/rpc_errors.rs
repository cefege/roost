//! Harness and login errors as Connect statuses: a missing conversation is
//! NotFound, a rejected argument InvalidArgument, a precondition (no model, no
//! account) FailedPrecondition, storage and provider failures Internal/Unavailable.

use connectrpc::{ConnectError, ErrorCode};
use roost_agent::AgentError;
use roost_llm::{CredentialStoreError, LlmError};

use super::LoginError;

pub fn agent_status(error: AgentError) -> ConnectError {
    let code = match &error {
        AgentError::NotFound(_) => ErrorCode::NotFound,
        AgentError::InvalidArgument(_) => ErrorCode::InvalidArgument,
        AgentError::FailedPrecondition(_) => ErrorCode::FailedPrecondition,
        AgentError::Model(_) => ErrorCode::Unavailable,
        AgentError::Cancelled => ErrorCode::Canceled,
        AgentError::Conflict(_) => ErrorCode::Aborted,
        AgentError::Fenced(_) => ErrorCode::Unavailable,
        AgentError::Store(_) => {
            tracing::error!(%error, "agent store failure");
            return ConnectError::new(ErrorCode::Internal, "agent storage failed");
        }
    };
    ConnectError::new(code, error.to_string())
}

pub fn login_status(error: LoginError) -> ConnectError {
    match error {
        LoginError::NotFound(_) => ConnectError::new(ErrorCode::NotFound, error.to_string()),
        LoginError::Provider(_) => {
            ConnectError::new(ErrorCode::FailedPrecondition, error.to_string())
        }
        LoginError::Storage(_) => {
            tracing::error!(%error, "agent login storage failure");
            ConnectError::new(ErrorCode::Internal, "agent login storage failed")
        }
    }
}

pub fn credential_status(error: CredentialStoreError) -> ConnectError {
    tracing::error!(%error, "agent credential storage failure");
    ConnectError::new(ErrorCode::Internal, "agent credential storage failed")
}

pub fn llm_status(error: LlmError) -> ConnectError {
    match error {
        LlmError::Storage(error) => credential_status(error),
        other => {
            tracing::error!(error = %other, "agent provider operation failed");
            ConnectError::new(ErrorCode::Unavailable, "agent provider operation failed")
        }
    }
}

pub fn internal(context: &str) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, context.to_owned())
}
