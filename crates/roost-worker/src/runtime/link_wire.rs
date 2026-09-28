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
//! It is also why this module holds an interface rather than a codec. The worker
//! crate does depend on `roost-proto` — the bootstrap redemption calls a
//! generated Connect service — so the generated message types are reachable from
//! here in principle. What is not permitted is the MAPPING: `roost-protocol`
//! owns the wire's definition, and a second one written against the union's own
//! field numbers would be a second definition that agrees with the first until
//! it does not. The interface keeps the codec swappable without reaching into
//! the owner's internals, and `tests/link_wire_parity.rs` is what keeps the
//! delegation honest — a second mapping would round-trip against itself
//! perfectly and still be wrong.

use roost_proto::CoordWorkerDown;
use roost_proto::buffa::Message as _;
use roost_proto::coord_worker_down::Frame;
use roost_protocol::proto_adapters::coord_worker_proto;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};

/// Why a frame could not be turned into bytes or back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("an upstream frame did not encode: {reason}")]
    Unencodable { reason: String },
    #[error("a downstream frame did not decode: {reason}")]
    Undecodable { reason: String },
    /// A well-formed `browser-command` envelope whose relayed frame does not
    /// parse. Distinct because it still carries the envelope's correlation,
    /// which v2 answers with `rpc-error "invalid browser command"`.
    #[error("browser command {request_id} did not parse: {reason}")]
    InvalidBrowserCommand { request_id: String, reason: String },
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
        coord_worker_proto::decode_downstream(bytes)
            .map_err(|error| classify_decode_failure(bytes, error.to_string()))
    }
}

/// Read only the envelope of a frame the mapping refused: when it is a
/// `browser-command`, only the relayed frame can have failed (its other fields
/// are free strings), and the envelope's `request_id` is still good to answer on.
fn classify_decode_failure(bytes: &[u8], reason: String) -> WireError {
    match CoordWorkerDown::decode_from_slice(bytes) {
        Ok(CoordWorkerDown {
            frame: Some(Frame::BrowserCommand(command)),
            ..
        }) => WireError::InvalidBrowserCommand {
            request_id: command.request_id,
            reason,
        },
        _ => WireError::Undecodable { reason },
    }
}
