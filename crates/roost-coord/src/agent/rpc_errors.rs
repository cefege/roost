//! Harness and login errors as Connect statuses: a missing conversation is
//! NotFound, a rejected argument InvalidArgument, a precondition (no model, no
//! account) FailedPrecondition, storage and provider failures Internal/Unavailable.

use connectrpc::{ConnectError, ErrorCode};
use roost_agent::AgentError;

use super::LoginError;

pub fn agent_status(error: AgentError) -> ConnectError {
    let code = match &error {
        AgentError::NotFound(_) => ErrorCode::NotFound,
        AgentError::InvalidArgument(_) => ErrorCode::InvalidArgument,
        AgentError::FailedPrecondition(_) => ErrorCode::FailedPrecondition,
        AgentError::Model(_) => ErrorCode::Unavailable,
        AgentError::Cancelled => ErrorCode::Canceled,
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
    }
}

pub fn internal(context: &str) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, context.to_owned())
}
