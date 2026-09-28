//! The Playwright oracle's backdoor, `window.__smoke`, compiled only with the
//! `smoke` feature and installed only when `localStorage.roostSmoke === "1"`.
//! Native modules own every rule (argument grammar, scans, proofs, flows,
//! ledgers) with tests; the wasm32 adapters bind them to the page. `App` calls
//! `install_smoke_backdoor` once. Ports `apps/web/src/smoke/*`.

pub mod call;
mod call_args;
pub mod created_resources;
pub mod dom_hold;
pub mod file_transfer;
pub mod harness;
pub mod marker_scan;
pub mod paint_proof;
pub mod probes;
pub mod retained_scan;
pub mod state_snapshot;
pub mod timing;

#[cfg(target_arch = "wasm32")]
mod backdoor;
#[cfg(target_arch = "wasm32")]
mod dispatch;
#[cfg(target_arch = "wasm32")]
mod dom;
#[cfg(target_arch = "wasm32")]
mod paint_wait;
#[cfg(target_arch = "wasm32")]
mod rpc_calls;

#[cfg(target_arch = "wasm32")]
pub use backdoor::install_smoke_backdoor;
