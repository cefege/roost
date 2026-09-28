//! The loopback door's shared pieces (v2 `local-door/local-ui-server.ts`): who
//! may knock ([`admission`]), the upgraded socket and the route owners it is
//! handed to ([`loopback`]), and the page bundle ([`spa`]). `runtime::door_serve`
//! binds and serves the door, `runtime::door_routes` answers its requests, and
//! `crate::local_terminal` owns what a terminal socket does once upgraded. The
//! browser-facing paths live in `roost_protocol::local_ui_door`.

pub mod admission;
pub mod loopback;
mod loopback_socket;
pub mod spa;
mod spa_cache;

pub use loopback_socket::{LoopbackSend, LoopbackSocket};
pub use roost_protocol::local_ui_door::{
    LOCAL_BOOTSTRAP_PATH, LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES, LOCAL_TERMINAL_MAX_PAYLOAD_BYTES,
    LOCAL_TERMINAL_PATH, LOCAL_TERMINAL_SUBPROTOCOL,
};

/// The attachment transfer socket's path.
///
/// The transfer protocol's own loopback constant rather than the terminal
/// socket's naming, because the browser reads one value for both doors.
pub const LOCAL_ATTACHMENT_PATH: &str = "/ws/local-attachment-transfer";

/// The attachment transfer socket's subprotocol.
pub const LOCAL_ATTACHMENT_SUBPROTOCOL: &str = "roost-local-attachment-transfer-v1";

/// The largest attachment frame the door hands to the attachment owner; a
/// larger one closes the socket before any decoding.
pub const LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
