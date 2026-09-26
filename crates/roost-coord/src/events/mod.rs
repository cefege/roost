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

// The write path, in the order the contract runs it. `append_input`,
// `append_transaction` and `append_publication` are **private modules on
// purpose**: the transaction body must not be able to name the publisher, and the
// publisher must not be reachable from a handler. That is §3.2's "the transaction
// body contains no publish call at all" made structural rather than reviewed.
//
// Nine files rather than v2's six, and every split is a boundary v2 already had
// somewhere: the bus and its payload vocabulary; the row shape and the statements
// that write it; the admission rules and the queries that fill their facts; and
// the three phases of the append path, which are the three phases of the
// contract.
mod admission_facts;
mod append_input;
mod append_publication;
mod append_transaction;

pub mod admission;
pub mod agent_conversation_recovery;
pub mod append;
pub mod bus;
pub mod bus_domains;
pub mod bus_messages;
pub mod event_log;
pub mod event_query;
pub mod pending_publications;
pub mod persistence_input;
pub mod projection;
pub mod projection_writes;
pub mod visibility;
