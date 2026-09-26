//! The client as a synchronous state machine: one `handle` that takes an event
//! and returns what should be sent, and a store it owns.
//!
//! No DOM type, no browser JS binding, no async runtime I/O type, and no timer.
//! The browser, a TUI and a mobile host drive the same code because every
//! platform capability is either a trait the core calls into synchronously
//! (`platform`) or an effect the host performs and reports back as an event
//! (`effect`, `event`).
//!
//! What this crate is FOR is the terminal cell protocol: exactly one complete
//! authoritative full before any delta, an epoch fence on every delta, and one
//! latched repair per gap. Those rules are in `docs/phase4-client-contract.md`
//! with their sources, and each has a test that fails if the rule is weakened.
//!
//! What it must never do: fold a session event (`roost-protocol` owns the one
//! fold), assemble a chunk (`roost-protocol` owns that), encode protobuf (the
//! host owns the wire), or mint a stream id (the view authority owns that).

#![forbid(unsafe_code)]

mod core;
mod handle_event;
mod handle_input;
mod handle_sweep;
mod handle_sync;
mod handle_terminal;

pub mod effect;
pub mod event;
pub mod platform;
pub mod search;
pub mod sessions;
pub mod store;
pub mod sync;
pub mod terminal;

pub use core::ClientCore;
pub use effect::{ChallengePurpose, DirectCommand, Effect, RpcCall, RpcResult, SyncCommand};
pub use event::ClientEvent;
pub use platform::{Clock, KeyValueStore, MemoryClock, MemoryKeyValueStore};
pub use search::{FindMatch, PageRefusal, RawMatch, SearchPage};
pub use sessions::{SessionPlane, WireEvent, WireSession};
pub use store::Store;
pub use sync::{DomainToken, SyncDial, SyncDomain, SyncFrame, SyncState};
pub use terminal::{
    Admission, DirectCarrier, FoldTarget, FrameFoldFailure, FrameFoldOutcome, HistoryRange,
    InputOutcome, InputPhase, InputRouter, PromotionCandidate, PromotionRefusal, RouteRegistry,
    TerminalSession, TerminalToken, TerminalTransport, TerminalView, ViewIntent,
    ViewStateAdmission, ViewStateResult,
};
