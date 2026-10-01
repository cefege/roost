//! The direct carrier's host side: the connections a document holds, and the
//! routing of one `Effect` onto the right one.
//!
//! Owned by `platform`, driven by `pump::effects`. It is the host half of the
//! seam `roost_client_core::client::carriers` draws: the core decides WHEN a
//! carrier may open, what it may carry, and when it is elected
//! (`Signalling`, `RouteRegistry`); this module opens sockets, spends grants,
//! moves bytes, and reports what it saw back as `ClientEvent`s.
//!
//! It deliberately holds no election policy of its own. There is exactly one
//! place that decides which carrier serves a session — `terminal::routes` — and
//! the only thing this module is allowed to answer is "which live connection
//! presents this exact token", because that is a lookup, not a decision.
//!
//! Submodules: `dial` resolves a minted grant against this page's door before
//! a socket exists, `loopback_carrier` is one loopback connection's handshake
//! and traffic, `peer_carrier` is one WebRTC carrier's lane framing, `route` is
//! the table that says which live connection presents a given generation, and
//! `send` is the decision one `SendDirect` becomes on it.

pub mod dial;
pub mod loopback_carrier;
pub mod peer_carrier;
pub mod route;
pub mod send;

pub use dial::{DialFault, DialPlan};
pub use loopback_carrier::{CarrierFault, LoopbackConnection};
pub use peer_carrier::{
    LaneFault, LaneMessage, PeerCarrier, PeerCarriers, PeerDeadline, PeerLanes, watermarks,
};
pub use route::{CarrierTable, ConnectionKey};
pub use send::delivery;
