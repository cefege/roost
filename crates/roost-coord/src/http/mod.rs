//! The HTTP listener and the routes it serves.
//!
//! Owned by the coordinator. `listener` owns the mounting and the ORDER the five
//! surfaces are tried in, which is itself part of the contract; `bind` owns the
//! bind lifecycle and `upgrade` owns the two WebSocket upgrade lifecycles, the
//! only handlers that read `sec-websocket-protocol` and authenticate a
//! credential from it. The decisions each upgrade makes live in
//! `worker_link::upgrade_admission` and `sync_ws::upgrade_admission`, and the
//! SQL lives in `db`. `spa` owns the browser's front door and every PATH
//! decision in it, which is `roost_host::spa_path` -- shared with the worker's
//! local door so the two front doors cannot disagree.
//!
//! The coordinator never terminates TLS: "The coordinator serves plaintext on its
//! loopback bind; the operator's front door owns TLS"
//! (`apps/coord/src/bun-coordinator-listeners.ts:4-5`).

pub mod bind;
pub mod db_export;
pub mod health;
pub mod listener;
pub mod spa;
pub mod spa_cache;
pub mod upgrade;
