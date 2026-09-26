//! The per-session terminal byte hub, the screen residency budget, and the
//! scrollback reads a browser drives against a worker.
//!
//! Owned by the coordinator's terminal-screen domain. `route_index` owns the
//! `(worker, channel) -> session` half; `byte_hub` owns the cells that arrive
//! on it. Neither names a caller, because `coord_core::seams::WorkerRouteIndex`
//! and the RPC layer are both consumers of this module and neither is a
//! dependency of it. `scrollback_*` is the demand-driven history read, which
//! runs on the worker and is relayed from here.

pub mod byte_hub;
pub mod pending_rpcs;
pub mod replica;
pub mod replica_admission;
pub mod residency;
pub mod route_index;
pub mod rpc;
pub mod rpc_relay;
pub mod screen_budget;
pub mod scrollback_relay;
pub mod scrollback_result;
pub mod scrollback_window;
pub mod search_ledger;

pub use byte_hub::ByteHub;
pub use replica::{NoScreenReplicaSink, ScreenHub, ScreenReplicaSink};
pub use residency::{ResidentCache, SessionCharge, TerminalScreenResidency};
pub use route_index::{NoRouteRetirement, RouteIndex, RouteRetirement, RouteRetirementSink};
pub use rpc::SCROLLBACK_METHODS;
pub use screen_budget::{
    TerminalScreenCaps, sync_backpressure_bytes, terminal_screen_budget_bytes, terminal_screen_caps,
};
pub use scrollback_relay::ScrollbackRelay;
