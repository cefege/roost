//! Port of `packages/agent/src/harness/session/` @ pi 0.82.1 (2efa728) — the
//! harness session layer: the [`Session`] facade (session.ts, implemented in
//! `session_facade`), the storage/repo implementations, and shared helpers.
//!
//! Module mapping (T16 plan):
//! - [`session_facade`] — `session/session.ts` (Session facade + context
//!   building; the type-layer `types::Session` trait holds the class surface).
//! - [`jsonl_storage`] / [`jsonl_repo`] — `session/jsonl-storage.ts` /
//!   `session/jsonl-repo.ts`.
//! - [`memory_storage`] / [`memory_repo`] — `session/memory-storage.ts` /
//!   `session/memory-repo.ts`.
//! - [`repo_utils`] — `session/repo-utils.ts` (shared helpers).

pub mod jsonl_repo;
pub mod jsonl_storage;
pub mod memory_repo;
pub mod memory_storage;
pub mod repo_utils;
pub mod session_facade;

/// The concrete `Session` facade struct (session.ts:150) — distinct from the
/// `types::Session` trait (re-exported at `crate::harness::Session`).
pub use session_facade::Session;
