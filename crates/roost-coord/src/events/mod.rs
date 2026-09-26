//! The durable session-event path: what may be appended, what may be published,
//! and the two are not the same question.
//!
//! Owned by the coordinator. `admission` decides whether an event may be written
//! and is pure over facts the I/O layer supplies; `pending_publications` recovers
//! a publication that was lost after the write committed; `visibility` is the one
//! public/private predicate every consumer shares.
//!
//! The commit itself -- insert with `ON CONFLICT (worker_fp, client_seq) DO
//! NOTHING`, then the `sessions` projection, then the publish strictly after
//! commit -- is the database layer's job and is specified in
//! `docs/phase3-coord-contract.md` §3.1.

pub mod admission;
pub mod pending_publications;
pub mod visibility;
