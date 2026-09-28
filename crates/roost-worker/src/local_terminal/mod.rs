//! The worker's direct terminal path: grants the coordinator installs, the
//! socket owner that admits loopback and peer carriers against them, and the
//! input, route-claim, probe and history controls those carriers reach the
//! PTYs through. Built by `runtime::owners` (`door::LocalTerminalDoor`); the
//! door's router and the peer owner drive `sockets::LocalTerminalSockets`.
//! Ports `apps/worker/src/local-door/local-terminal-*.ts`.

mod authority;
mod controls;
mod delivery;
mod door;
mod grant_scope;
mod grant_state;
mod grants;
mod hello;
mod input;
mod loopback;
mod port;
mod scrollback;
mod sockets;

pub use door::{LocalTerminalDoor, LocalTerminalDoorDeps};
pub use grant_scope::{
    GrantChange, GrantCredential, GrantRemovalReason, LocalTerminalGrant, MAX_GRANTS, MAX_TTL_MS,
    PeerGrantAuthorization,
};
pub use grants::{GrantListener, LocalTerminalGrantStore};
pub use loopback::LoopbackTerminalPacketPort;
pub use port::{
    ExpectedPeer, HistoryReadReservation, PacketSendResult, PeerTerminalPacketPort,
    TerminalPacketPort,
};
pub use scrollback::read_local_scrollback;
pub use sockets::{LocalTerminalSockets, LocalTerminalSocketsDeps, PeerIngress};
