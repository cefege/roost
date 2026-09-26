//! The binary frame codec for the coordinator link, and the honest absence of
//! one. Called by the link loop for every frame in both directions.
//!
//! The link carries protobuf `CoordWorkerUp` / `CoordWorkerDown` messages as
//! binary WebSocket messages. The typed unions for those two live in
//! `roost-protocol` (`wire::coord_worker`), which is where the domain view of a
//! frame belongs; turning one into bytes is a mapping, and `roost-protocol`
//! already owns that job for events, sessions and keeper reports under
//! `proto_adapters`.
//!
//! It is also why this module holds an interface rather than a codec. The
//! worker crate is not allowed to depend on `roost-proto` — the generated
//! types reach it only through `roost-protocol` — so a codec written here could
//! not name the messages it encodes.

use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};

/// Why a frame could not be turned into bytes or back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("the coordinator link has no frame codec: {reason}")]
    Unavailable { reason: String },
    #[error("an upstream frame did not encode: {reason}")]
    Unencodable { reason: String },
    #[error("a downstream frame did not decode: {reason}")]
    Undecodable { reason: String },
}

/// Turns typed link frames into the bytes on the socket and back.
pub trait LinkWire: Send + Sync {
    /// Encode one upstream frame.
    fn encode_upstream(&self, frame: &CoordWorkerUpstream) -> Result<Vec<u8>, WireError>;
    /// Decode one downstream frame.
    fn decode_downstream(&self, bytes: &[u8]) -> Result<CoordWorkerDownstream, WireError>;
}

/// The codec the service installs, which cannot encode yet.
///
/// UNIMPLEMENTED: add `roost_protocol::proto_adapters::coord_worker_proto`
/// over `roost_proto::roost::v1::{CoordWorkerUp, CoordWorkerDown}` — the
/// generated pair `protocol/proto/roost/v1/worker_transport.proto` already
/// defines — and implement this trait over it. `CoordWorkerUp` has 19 arms and
/// `CoordWorkerDown` has 28, so that module is a mapping and belongs in
/// `roost-protocol`, not here.
///
/// A second gap sits underneath it and is the one that actually blocks the
/// barrier: the ported `CoordWorkerDownstream` union carries only `hello-ack`,
/// `ping` and `browser-command`, while the proto's `CoordWorkerDown` also has
/// `event_ack` (field 5) and `terminal_snapshot_request` (field 14). Those two
/// are the only frames `Pump::on_event_ack` and `Pump::on_snapshot_ack` can be
/// driven by, so the union needs those arms before the barrier can be released
/// past `snapshot` by anything on the wire.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableWire;

impl LinkWire for UnavailableWire {
    fn encode_upstream(&self, _frame: &CoordWorkerUpstream) -> Result<Vec<u8>, WireError> {
        Err(WireError::Unavailable {
            reason: "the CoordWorkerUp codec is not ported yet".to_string(),
        })
    }

    fn decode_downstream(&self, _bytes: &[u8]) -> Result<CoordWorkerDownstream, WireError> {
        Err(WireError::Unavailable {
            reason: "the CoordWorkerDown codec is not ported yet".to_string(),
        })
    }
}
