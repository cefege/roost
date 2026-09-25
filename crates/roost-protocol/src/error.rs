//! The one error type every validation in this crate returns.
//!
//! A rejected value is never a panic and never a silently dropped event: the
//! coordinator's worker link, the browser's Sync socket, and the keeper's
//! admission path all need to tell "this peer sent something outside the
//! contract" apart from "this peer sent something we do not understand", and
//! both apart from success.

use std::fmt;

/// Where a value failed the contract, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    /// The dotted path of the offending field, e.g. `snapshot.sessions[2].spans[0].columns`.
    pub field: String,
    /// Why it failed, phrased so it can go straight into a log line.
    pub reason: String,
}

impl ProtocolError {
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// Prefix the field path, for a validator that delegates to a sub-validator.
    pub fn within(mut self, prefix: &str) -> Self {
        self.field = format!("{prefix}.{}", self.field);
        self
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.reason)
    }
}

impl std::error::Error for ProtocolError {}

/// The result of a contract check.
pub type ProtocolResult<T> = Result<T, ProtocolError>;
