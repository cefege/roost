//! The binary codec for the coordinator-worker link: `CoordWorkerUp` and
//! `CoordWorkerDown` as bytes, in both directions. Called by the coordinator's
//! worker WS handler and by the worker's `LinkWire`.
//!
//! Each arm is the matching variant of the generated oneof, so no field number
//! is written here: every one is the number
//! `protocol/proto/roost/v1/worker_transport.proto` compiled into the
//! generated code, and the per-arm mappings live in `upstream`, `downstream`
//! and `records` beside the arms they build. A field number this build does not
//! know is REFUSED with that number in the message rather than skipped: on this
//! socket a dropped arm is a dropped terminal frame, and a coordinator that
//! silently discards one reports a session as live that has stopped painting.
//!
//! Payloads with a domain owner in `roost_protocol::wire` are mapped through
//! it, so this codec never becomes a second definition of an event, a control
//! frame or a status. The arms with no owner yet carry their generated message
//! unchanged, which is the same trade the union makes.

use roost_proto::buffa::{DecodeError, Message};
use roost_proto::{CoordWorkerDown, CoordWorkerUp};

use crate::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use crate::{ProtocolError, ProtocolResult};

mod control_outcomes;
mod downstream;
mod records;
mod upstream;

const UPSTREAM: &str = "coord_worker_upstream";
const DOWNSTREAM: &str = "coord_worker_downstream";

/// The oneof field this frame arrived under is not an arm this build names.
/// Named with its number AND its direction: a refusal a reader cannot place on
/// the wire is a refusal nobody can act on.
fn unknown_arm(direction: &str, number: u32) -> ProtocolError {
    ProtocolError::new(
        direction,
        format!("oneof field {number} is not an arm this build knows"),
    )
}

/// The oneof carried no arm at all, which is a frame with nothing in it rather
/// than a frame whose arm went missing.
fn empty_arm(direction: &str) -> ProtocolError {
    ProtocolError::new(direction, "carried no frame arm")
}

/// Encode one worker-to-coordinator frame as its protobuf bytes.
pub fn encode_upstream(frame: &CoordWorkerUpstream) -> ProtocolResult<Vec<u8>> {
    Ok(upstream::to_proto(frame)?.encode_to_vec())
}

/// Decode one worker-to-coordinator frame from its protobuf bytes.
pub fn decode_upstream(bytes: &[u8]) -> ProtocolResult<CoordWorkerUpstream> {
    let message = CoordWorkerUp::decode_from_slice(bytes)
        .map_err(|error: DecodeError| ProtocolError::new(UPSTREAM, error.to_string()))?;
    upstream::from_proto(&message)
}

/// Encode one coordinator-to-worker frame as its protobuf bytes.
pub fn encode_downstream(frame: &CoordWorkerDownstream) -> ProtocolResult<Vec<u8>> {
    Ok(downstream::to_proto(frame)?.encode_to_vec())
}

/// Decode one coordinator-to-worker frame from its protobuf bytes.
pub fn decode_downstream(bytes: &[u8]) -> ProtocolResult<CoordWorkerDownstream> {
    let message = CoordWorkerDown::decode_from_slice(bytes)
        .map_err(|error: DecodeError| ProtocolError::new(DOWNSTREAM, error.to_string()))?;
    downstream::from_proto(&message)
}
