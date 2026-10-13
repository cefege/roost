//! What lets several coordinator replicas share one Postgres database: the
//! cross-replica notice channel. Every replica is equal; ownership of a
//! conversation's run is a lease row, not a property of a process.

pub mod notify;
