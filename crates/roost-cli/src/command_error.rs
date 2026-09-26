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

/// A usage error the operator has to fix before anything can run.
pub const USAGE_FAILURE: u8 = 2;

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
        Self::new(USAGE_FAILURE, message)
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
macro_rules! generic_failure_from {
    ($($source:ty),* $(,)?) => {
        $(
            impl From<$source> for CommandFailure {
                fn from(error: $source) -> Self {
                    CommandFailure::generic(error.to_string())
                }
            }
        )*
    };
}

generic_failure_from!(
    anyhow::Error,
    std::io::Error,
    serde_json::Error,
    roost_host::ProtocolError,
    roost_worker::runtime::boot::BootConfigError,
    crate::status::collect::CollectError,
    crate::status::inventory::InventoryError,
    reqwest::Error,
);

#[cfg(test)]
mod tests {
    use super::{CommandFailure, GENERIC_FAILURE, USAGE_FAILURE};

    #[test]
    fn a_failure_keeps_the_code_it_was_given() {
        let failure = CommandFailure::new(5, "keeper not adoptable");
        assert_eq!(failure.code, 5);
        assert_eq!(failure.to_string(), "keeper not adoptable");
    }

    #[test]
    fn the_reserved_codes_are_distinct() {
        assert_ne!(GENERIC_FAILURE, USAGE_FAILURE);
    }
}
