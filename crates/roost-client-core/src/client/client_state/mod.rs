//! Account-scoped client state sync: the browser keys that follow the operator
//! across laptops, merged by last-writer-wins on epoch stamps.
//!
//! `store` wraps the local key/value store and records what changed; `sync`
//! pulls, debounces and flushes from the sweep and settles the landing gate;
//! `merge` applies coordinator answers. `roost-protocol::client_state` alone
//! decides which keys sync.

mod entry;
mod merge;
mod stamps;
mod store;
mod sync;

pub use entry::ClientStateEntry;
pub use store::ClientStateStore;
pub use sync::ClientStateSync;
pub(crate) use sync::{request_list, settle_result, sweep};
