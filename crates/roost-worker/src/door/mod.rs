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
/// The attachment transfer socket's path, subprotocol and largest frame the
/// door hands to the attachment owner (a larger one closes the socket before
/// any decoding); the transfer protocol's own constants, because the browser
/// reads one value for both doors (v2 `local-ui-server.ts:60-63`).
pub use roost_protocol::attachment_transfer::{
    LOOPBACK_MAX_PAYLOAD_BYTES as LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES,
    LOOPBACK_PATH as LOCAL_ATTACHMENT_PATH, LOOPBACK_SUBPROTOCOL as LOCAL_ATTACHMENT_SUBPROTOCOL,
};
