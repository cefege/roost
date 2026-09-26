//! The HTTP listener and the routes it serves.
//!
//! Owned by the coordinator. `listener` owns the mounting and the ORDER the five
//! surfaces are tried in, which is itself part of the contract; the decisions each
//! upgrade makes live in `worker_link::upgrade_admission` and
//! `sync_ws::upgrade_admission`, and the SQL lives in `db`.
//!
//! The coordinator never terminates TLS: "The coordinator serves plaintext on its
//! loopback bind; the operator's front door owns TLS"
//! (`apps/coord/src/bun-coordinator-listeners.ts:4-5`).

pub mod listener;
