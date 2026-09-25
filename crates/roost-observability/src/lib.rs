//! tracing JSON-lines initialization and the structured event vocabulary. Every crate that logs depends on it. Owns the log line shape that roost status and roost doctor parse.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]
