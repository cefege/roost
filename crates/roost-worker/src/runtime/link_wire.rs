//! The binary frame codec for the coordinator link. Called by the link loop for
//! every frame in both directions, and by nothing else.
//!
//! The link carries protobuf `CoordWorkerUp` / `CoordWorkerDown` messages as
//! binary WebSocket messages. The typed unions for those two live in
//! `roost-protocol` (`wire::coord_worker`), which is where the domain view of a
//! frame belongs; turning one into bytes is a mapping, and
//! [`roost_protocol::proto_adapters::coord_worker_proto`] already owns that job
//! for all 19 upstream and 28 downstream arms.
//!
//! It is also why this module holds an interface rather than a codec. The
//! worker crate is not allowed to depend on `roost-proto` — the generated
//! types reach it only through `roost-protocol` — so a codec written here could
//! not name the messages it encodes. The interface is what makes the mapping
//! swappable at all, and the test fakes in
//! `crates/roost-worker/tests/worker_reconnect_ladder.rs` are the other
//! implementors it exists for.

use roost_protocol::proto_adapters::coord_worker_proto;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};

/// Why a frame could not be turned into bytes or back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
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

/// The codec the service installs: the protobuf mapping in `roost-protocol`.
///
/// The mapping is not written here and must not be. A second one, written
/// against the union's own field numbers, would be a second definition of the
/// wire that agrees with the first until it does not — and on this socket a
/// dropped arm is a dropped terminal frame, which the coordinator reports as a
/// session that is live and has stopped painting.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProtoLinkWire;

impl LinkWire for ProtoLinkWire {
    fn encode_upstream(&self, frame: &CoordWorkerUpstream) -> Result<Vec<u8>, WireError> {
        coord_worker_proto::encode_upstream(frame).map_err(|error| WireError::Unencodable {
            reason: error.to_string(),
        })
    }

    fn decode_downstream(&self, bytes: &[u8]) -> Result<CoordWorkerDownstream, WireError> {
        coord_worker_proto::decode_downstream(bytes).map_err(|error| WireError::Undecodable {
            reason: error.to_string(),
        })
    }
}
