//! Every arm of both worker-link oneofs lands on the field number
//! `protocol/proto/roost/v1/worker_transport.proto` declares for it. The
//! numbers are read off the ENCODED tag rather than off the union, so this is a
//! check on the bytes and not a restatement of the mapping the codec performs.
//!
//! A behaviour test unwraps the value it is asserting about: a failure there is
//! the assertion failing, which is exactly what a test wants.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "coord_worker_link/arms.rs"]
mod arms;
#[path = "coord_worker_link/fixtures.rs"]
mod fixtures;

use arms::{downstream_arms, upstream_arms};
use roost_protocol::proto_adapters::coord_worker_proto::{encode_downstream, encode_upstream};

/// `CoordWorkerUp`'s oneof arms and the field numbers the proto gives them.
const UPSTREAM_FIELD_NUMBERS: &[(&str, u32)] = &[
    ("hello", 1),
    ("pong", 2),
    ("event", 3),
    ("rpc-ok", 6),
    ("rpc-error", 7),
    ("binary", 10),
    ("refresh-jwt", 11),
    ("cell-grid", 12),
    ("agent-status", 14),
    ("input-result", 16),
    ("terminal-stream-result", 17),
    ("cell-grid-chunk", 18),
    ("terminal-pipeline-snapshot", 19),
    ("terminal-metadata", 20),
    ("terminal-view-state", 21),
    ("terminal-view-projection", 22),
    ("local-terminal-peer-answer", 23),
    ("local-terminal-peer-error", 24),
    ("terminal-input-route-result", 25),
    ("terminal-transport-probe-result", 26),
    ("local-attachment-peer-answer", 27),
    ("local-attachment-peer-error", 28),
    ("attachment-direct-status", 29),
    ("update-progress", 30),
];

/// `CoordWorkerDown`'s oneof arms. Field 11 is `reserved`, so a peer that
/// still sends it is refused by number rather than read as an arm.
const DOWNSTREAM_FIELD_NUMBERS: &[(&str, u32)] = &[
    ("hello-ack", 1),
    ("ping", 2),
    ("browser-command", 3),
    ("binary", 4),
    ("event-ack", 5),
    ("attachment-chunk", 6),
    ("coord-move-prepare", 7),
    ("coord-move-snapshot-start", 8),
    ("coord-move-snapshot-chunk", 9),
    ("coord-relocate", 10),
    ("input-request", 12),
    ("terminal-stream-state", 13),
    ("terminal-snapshot-request", 14),
    ("keeper-update-prepare", 15),
    ("agent-prompt", 16),
    ("terminal-pipeline-snapshot", 17),
    ("terminal-view-relay", 18),
    ("terminal-view-socket-closed", 19),
    ("local-terminal-grant", 20),
    ("local-terminal-grant-revoke", 21),
    ("local-terminal-peer-offer", 22),
    ("local-terminal-peer-cancel", 23),
    ("terminal-input-route-claim", 24),
    ("terminal-transport-probe", 25),
    ("terminal-direct-retire", 26),
    ("local-attachment-peer-offer", 27),
    ("local-attachment-peer-cancel", 28),
    ("local-attachment-grant", 29),
    ("update-broker", 30),
    ("local-attachment-grant-revoke", 31),
    ("attachment-direct-status-request", 32),
];

/// The field number of a length-delimited protobuf field, read off the wire.
/// Every arm of both oneofs is length-delimited, so the low three bits are the
/// wire type and the rest is the number.
fn leading_field_number(bytes: &[u8]) -> u32 {
    let mut tag = 0u64;
    for (index, byte) in bytes.iter().enumerate() {
        tag |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            break;
        }
    }
    assert_eq!(tag & 0b111, 2, "a oneof arm is always length-delimited");
    (tag >> 3) as u32
}

fn declared(table: &[(&str, u32)], kind: &str) -> u32 {
    table
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, number)| *number)
        .unwrap_or_else(|| panic!("{kind} has no field number in the proto"))
}

#[test]
fn every_upstream_arm_lands_on_the_field_number_the_proto_declares() {
    for (kind, frame) in upstream_arms() {
        let bytes = encode_upstream(&frame).expect("the arm encodes");
        assert_eq!(
            leading_field_number(&bytes),
            declared(UPSTREAM_FIELD_NUMBERS, kind),
            "{kind} encoded under the wrong oneof field number"
        );
    }
}

#[test]
fn every_downstream_arm_lands_on_the_field_number_the_proto_declares() {
    for (kind, frame) in downstream_arms() {
        let bytes = encode_downstream(&frame).expect("the arm encodes");
        assert_eq!(
            leading_field_number(&bytes),
            declared(DOWNSTREAM_FIELD_NUMBERS, kind),
            "{kind} encoded under the wrong oneof field number"
        );
    }
}
