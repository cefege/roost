//! The WebRTC terminal peer: the str0m side of a browser reaching this
//! worker's PTYs over negotiated data channels rather than the coordinator
//! link. `crate::local_terminal` owns the frames; this owns the negotiated
//! carrier — offer/answer, packet framing from `roost_protocol::terminal_peer`,
//! per-lane budgets and the history reservation. Built by `runtime::owners`
//! ([`DirectTerminal`]); ports v2 `apps/worker/src/terminal/peer/*`.

mod config;
mod connection;
mod coordinator_generation;
mod direct;
mod faults;
mod history_reservation;
pub mod native;
mod owner;
mod owner_offer;
mod packet_budget;
mod packet_lanes;
mod packet_port;
mod peer_budget;
mod request_validation;

pub use config::{
    PeerConfigError, PeerTransportConfig, TERMINAL_PEER_BIND_ADDRESS_ENV,
    TERMINAL_PEER_ENABLED_ENV, TERMINAL_PEER_PORT_RANGE_ENV,
};
pub use connection::{
    ConnectionFailure, OpenTerminalPeerPort, TerminalPeerConnection, TerminalPeerConnectionConfig,
    TerminalPeerConnectionDeps,
};
pub use coordinator_generation::CoordinatorGeneration;
pub use direct::{
    DirectCarrier, DirectLinkLifecycle, DirectPeerSupport, DirectTerminal, DirectTerminalDeps,
};
pub use faults::{OfferFault, OfferFaultSlot};
pub use owner::{
    PeerBootstrapState, TerminalPeerOfferFailure, TerminalPeerOwner, TerminalPeerOwnerDeps,
};
pub use packet_budget::{PacketBudgetSnapshot, PacketDirection, TerminalPeerPacketBudget};
pub use packet_port::{
    PacketPortDeps, PortHook, TerminalPeerPacketIngress, TerminalPeerPacketPort,
};
pub use peer_budget::{PeerBudgetHold, TerminalPeerPacketPeerBudget, TerminalPeerQuota};
pub use request_validation::valid_terminal_peer_offer_identity;
