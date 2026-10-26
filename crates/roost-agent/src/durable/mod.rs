//! Pi Durable's Rust kernel behind Roost's native agent interface.
//! The driver installs product policies and remote tools; storage atomically
//! settles kernel records with the existing chat projection and owner fence.

mod branch;
mod cancellation;
mod compaction;
mod driver;
mod hooks;
mod journal_sync;
mod live;
mod message;
mod model;
mod nested_projection;
mod policy;
mod projection;
mod prompts;
mod state;
mod storage;
mod tools;

pub(crate) use branch::fork_documents;
pub(crate) use driver::DurableDriver;
pub use rpi_durable::StorageWrite;
pub use storage::{DurableBatch, DurableCommit};

#[cfg(test)]
mod model_tests;
