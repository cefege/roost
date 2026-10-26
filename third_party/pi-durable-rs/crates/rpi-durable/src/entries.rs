//! Built-in entry kinds (port of `packages/durable/src/entries.ts` @ `abe508e1b`).
//!
//! An entry is one immutable transcript event ([`crate::types::EntryRecord`]).
//! `model` carries the messages the entry contributes to model context; `data`
//! carries application-facing payloads. The typed tokens upstream narrow the
//! `data` shape; the Rust port keeps plain constants and callers deserialize
//! the payload themselves.

/// User input; `model` is a one-element user-message list. Written by submissions.
pub const USER_ENTRY: &str = "pi.user";
/// Provider result with any stop reason; `model` is a one-element assistant-message list.
pub const ASSISTANT_ENTRY: &str = "pi.assistant";
/// Positional prompt and tool change; `model` is a system message with empty content.
pub const SYSTEM_ENTRY: &str = "pi.system";
/// Tool result; `model` is a one-element tool-result-message list, `data` holds diagnostics.
pub const TOOL_RESULT_ENTRY: &str = "pi.tool-result";
/// Start of a new context; always `head: "self"`, optionally carrying handoff text.
pub const RESET_ENTRY: &str = "pi.reset";
/// Compaction summary; `model` is a one-element user-message list, `head` the first kept entry.
pub const COMPACTION_ENTRY: &str = "pi.compaction";

/// Upstream `defineEntry` validation: a non-empty kind.
pub fn validate_entry_kind(kind: &str) -> Result<(), &'static str> {
    if kind.is_empty() {
        Err("Entry kind must be a non-empty string")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_kinds_are_non_empty() {
        for kind in [
            USER_ENTRY,
            ASSISTANT_ENTRY,
            SYSTEM_ENTRY,
            TOOL_RESULT_ENTRY,
            RESET_ENTRY,
            COMPACTION_ENTRY,
        ] {
            validate_entry_kind(kind).unwrap();
        }
        assert!(validate_entry_kind("").is_err());
    }
}
