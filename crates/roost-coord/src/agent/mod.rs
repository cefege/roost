//! Coordinator-owned calls to worker-local agent tools.
//!
//! `ToolCallRegistry` routes each call over the authenticated worker generation,
//! retains its output and completion senders, and rejects them when that generation ends.

pub mod tool_calls;
