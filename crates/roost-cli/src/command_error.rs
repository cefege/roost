//! The one error type every `roost <command>` returns, and the exit-code
//! contract it carries. Called by every command module in this crate and
//! printed by main.rs as the single `{"cmd":…,"error":…}` line stderr carries.
//!
//! The exit code is part of the value rather than something main.rs guesses,
//! because the codes are a contract: `roost deploy` reserves 5 for a keeper
//! that cannot be adopted safely, `roost doctor` reserves 2 for a malformed
//! `--since`, and a script that cannot tell those apart from a generic failure
//! is a script that retries the one thing that must never be retried.
//! docs/phase6-cli-contract.md lists every code and its cause.

use std::fmt;

/// A command that could not do what it was asked, and the code it exits with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFailure {
    pub code: u8,
    pub message: String,
}

/// The default: something went wrong and nothing more specific is known. Every
/// error type without a reserved code of its own becomes this, which is why a
/// bare `1` is still a real answer rather than a shrug.
pub const GENERIC_FAILURE: u8 = 1;

/// The invocation was rejected and nothing was attempted: a bad flag, a
/// malformed argument, an unknown verb, a malformed `--since`, or a deploy
/// whose target could not be reached. **The one definition of 2 in this
/// crate** — `deploy::codes` re-exports it rather than restating it, because
/// two constants carrying one value are two answers to "what does exit 2
/// mean", and a wrapper script has to act on the same answer either way.
pub const REJECTED_INVOCATION: u8 = 2;

impl CommandFailure {
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn generic(message: impl Into<String>) -> Self {
        Self::new(GENERIC_FAILURE, message)
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(REJECTED_INVOCATION, message)
    }
}

impl fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for CommandFailure {}

/// Every fallible step outside a command's own logic lands here with the
/// generic code. The `From` impls exist so a command body reads as its own
/// steps rather than as a chain of `map_err`s that all say the same thing.
///
/// Written out one by one rather than generated: this repository forbids
/// `macro_rules!`, and `cargo xtask lint` does not check for it, so a macro
/// here is a rule violation no gate would ever report. This was the only one
/// in the crate.
impl From<anyhow::Error> for CommandFailure {
    fn from(error: anyhow::Error) -> Self {
        CommandFailure::generic(format!("{error:#}"))
    }
}

impl From<std::io::Error> for CommandFailure {
    fn from(error: std::io::Error) -> Self {
        CommandFailure::generic(error.to_string())
    }
}

impl From<serde_json::Error> for CommandFailure {
    fn from(error: serde_json::Error) -> Self {
        CommandFailure::generic(error.to_string())
    }
}

impl From<roost_host::ProtocolError> for CommandFailure {
    fn from(error: roost_host::ProtocolError) -> Self {
        CommandFailure::generic(error.to_string())
    }
}

impl From<roost_worker::runtime::boot::BootConfigError> for CommandFailure {
    fn from(error: roost_worker::runtime::boot::BootConfigError) -> Self {
        CommandFailure::generic(error.to_string())
    }
}

impl From<crate::status::collect::CollectError> for CommandFailure {
    fn from(error: crate::status::collect::CollectError) -> Self {
        CommandFailure::generic(error.to_string())
    }
}

impl From<crate::status::inventory::InventoryError> for CommandFailure {
    fn from(error: crate::status::inventory::InventoryError) -> Self {
        CommandFailure::generic(error.to_string())
    }
}

impl From<reqwest::Error> for CommandFailure {
    fn from(error: reqwest::Error) -> Self {
        CommandFailure::generic(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandFailure, GENERIC_FAILURE, REJECTED_INVOCATION};

    #[test]
    fn a_failure_keeps_the_code_it_was_given() {
        let failure = CommandFailure::new(5, "keeper not adoptable");
        assert_eq!(failure.code, 5);
        assert_eq!(failure.to_string(), "keeper not adoptable");
    }

    #[test]
    fn the_reserved_codes_are_distinct() {
        assert_ne!(GENERIC_FAILURE, REJECTED_INVOCATION);
    }
}
