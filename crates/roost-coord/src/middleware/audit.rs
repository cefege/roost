//! The per-request audit hook and the `audit_bus` publish, kept apart from
//! the admission layer that calls it so the write path is testable without a
//! socket. Owned by the audit slice; the module declaration lives in
//! `middleware/mod.rs`.
