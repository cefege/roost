//! The loopback door: the HTTP and WebSocket surface a browser on this machine
//! uses to reach its own PTYs without the coordinator. `runtime::serve` binds
//! the listener, and `crate::local_door` decides who may hold a socket open.
//! Depends on `roost_protocol::local_ui_door` for the bind and origin — and on
//! nothing here.
//!
//! THE PATHS AND SUBPROTOCOLS LIVE HERE because a browser and this worker have
//! to agree on them and there is no third place they could agree instead. The
//! POLICY is not here: `crate::local_door` owns the pre-hello deadline, the
//! established-socket cap and the grant digests, and it is a pure decision
//! precisely so it can be tested without a socket, an origin, or a browser.

/// The WebSocket subprotocol a local terminal socket negotiates.
///
/// Distinct from the peer transport's, and it has to be: a loopback socket is
/// authenticated by a grant digest and a peer socket by a WebRTC grant, and a
/// loopback client that fell back to the peer's name would be refused for
/// arriving on the wrong lane.
pub const LOCAL_TERMINAL_SUBPROTOCOL: &str = "roost-local-terminal";

/// The local terminal socket's path.
pub const LOCAL_TERMINAL_PATH: &str = "/ws/local-terminal";

/// The bootstrap a local browser fetches before it has anything else: where the
/// coordinator is, and which worker this machine is.
pub const LOCAL_BOOTSTRAP_PATH: &str = "/api/local-bootstrap";

/// The attachment transfer socket's path, and the subprotocol it negotiates.
///
/// Both are the transfer protocol's own loopback constants rather than the
/// terminal socket's naming, because the browser reads one value for both
/// doors. A Rust owner for them does not exist yet: when the protocol grows
/// `attachment_transfer`, these two move there and this module re-exports
/// them rather than keeping a second copy.
pub const LOCAL_ATTACHMENT_PATH: &str = "/ws/local-attachment-transfer";

/// The attachment transfer socket's subprotocol.
pub const LOCAL_ATTACHMENT_SUBPROTOCOL: &str = "roost-local-attachment-transfer-v1";

/// The largest legitimate client frame on a local terminal socket.
///
/// One 64 KiB paste inside a protobuf envelope, with headroom for the view and
/// scrollback commands, and a ceiling that keeps a compromised page from
/// queueing megabyte frames into the worker's own loop.
pub const LOCAL_TERMINAL_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// How much a local socket may have queued before its writes stop being read.
pub const LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES: usize = 4 * 1024 * 1024;
