//! The coordinator-worker link codec: every arm of both oneofs survives a
//! round trip byte for byte, the bytes are the ones the TypeScript stack
//! produced, and a frame this build cannot name is refused rather than
//! skipped. The fixtures and the TypeScript byte constants are in
//! `coord_worker_link/`; the per-arm field numbers the proto
//! declares are asserted by `coord_worker_proto_fields.rs`.
//!
//! A behaviour test unwraps the value it is asserting about: a failure there is
//! the assertion failing, which is exactly what a test wants. The workspace
//! denies `unwrap`/`expect` because a panic on a bad wire value in a running
//! component is a fleet-visible outage, and that reasoning does not reach here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "coord_worker_link/arms.rs"]
mod arms;
#[path = "coord_worker_link/fixtures.rs"]
mod fixtures;

use arms::{downstream_arms, upstream_arms};
use fixtures::{
    SESSION, STREAM, TS_EVENT_ACK, TS_HELLO, TS_HELLO_ACK, TS_PONG, TS_SNAPSHOT_REQUEST, hello,
    hex, session, unknown_arm_frame,
};
use roost_protocol::proto_adapters::coord_worker_proto::{
    decode_downstream, decode_upstream, encode_downstream, encode_upstream,
};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, CoordWorkerUpstream, EventAck, TerminalSnapshotRequest,
};

#[test]
fn every_upstream_arm_round_trips_to_the_same_bytes() {
    for (kind, frame) in upstream_arms() {
        let first =
            encode_upstream(&frame).unwrap_or_else(|error| panic!("{kind} encodes: {error}"));
        let decoded =
            decode_upstream(&first).unwrap_or_else(|error| panic!("{kind} decodes: {error}"));
        assert_eq!(
            decoded.kind(),
            kind,
            "{kind} came back under another discriminant"
        );
        let second =
            encode_upstream(&decoded).unwrap_or_else(|error| panic!("{kind} re-encodes: {error}"));
        assert_eq!(
            hex(&second),
            hex(&first),
            "{kind} did not survive the round trip byte for byte"
        );
    }
}

#[test]
fn every_downstream_arm_round_trips_to_the_same_bytes() {
    for (kind, frame) in downstream_arms() {
        let first =
            encode_downstream(&frame).unwrap_or_else(|error| panic!("{kind} encodes: {error}"));
        let decoded =
            decode_downstream(&first).unwrap_or_else(|error| panic!("{kind} decodes: {error}"));
        assert_eq!(
            decoded.kind(),
            kind,
            "{kind} came back under another discriminant"
        );
        let second = encode_downstream(&decoded)
            .unwrap_or_else(|error| panic!("{kind} re-encodes: {error}"));
        assert_eq!(
            hex(&second),
            hex(&first),
            "{kind} did not survive the round trip byte for byte"
        );
    }
}

#[test]
fn the_upstream_hello_is_byte_identical_to_the_typescript_encoders() {
    // The fixture's value minus the trace id: the proto's `WHello` has no
    // trace field, so a hello that carried one could not be byte-compared with
    // what bufbuild emits.
    let mut frame = hello();
    if let CoordWorkerUpstream::Hello { trace_id, .. } = &mut frame {
        *trace_id = None;
    }
    assert_eq!(
        hex(&encode_upstream(&frame).expect("the hello encodes")),
        TS_HELLO
    );
}

#[test]
fn the_upstream_pong_is_byte_identical_to_the_typescript_encoders() {
    let frame = CoordWorkerUpstream::Pong {
        ts: 1_700_000_000_001,
        trace_id: None,
    };
    assert_eq!(
        hex(&encode_upstream(&frame).expect("the pong encodes")),
        TS_PONG
    );
}

#[test]
fn the_downstream_hello_ack_is_byte_identical_to_the_typescript_encoders() {
    let frame = CoordWorkerDownstream::HelloAck {
        capabilities: vec!["events-v1".to_owned(), "cell-shipping-v2".to_owned()],
        trace_id: None,
    };
    assert_eq!(
        hex(&encode_downstream(&frame).expect("the ack encodes")),
        TS_HELLO_ACK
    );
}

#[test]
fn the_downstream_event_ack_is_byte_identical_to_the_typescript_encoders() {
    let frame = CoordWorkerDownstream::EventAck(EventAck { client_seq: 42 });
    assert_eq!(
        hex(&encode_downstream(&frame).expect("the ack encodes")),
        TS_EVENT_ACK
    );
}

#[test]
fn the_downstream_snapshot_request_is_byte_identical_to_the_typescript_encoders() {
    let frame = CoordWorkerDownstream::TerminalSnapshotRequest(TerminalSnapshotRequest {
        session_id: session(SESSION),
        stream_id: STREAM.to_owned(),
    });
    assert_eq!(
        hex(&encode_downstream(&frame).expect("the request encodes")),
        TS_SNAPSHOT_REQUEST
    );
}

#[test]
fn an_upstream_arm_this_build_does_not_know_is_refused_by_its_field_number() {
    // 13 is `reserved` in CoordWorkerUp: a peer that still sends it is from a
    // build this one has to be able to say so about by number.
    let error =
        decode_upstream(&unknown_arm_frame(13)).expect_err("field 13 is reserved, not an arm");
    assert!(
        error.reason.contains("13"),
        "the refusal must name the field it could not place, got: {}",
        error.reason
    );
    assert_eq!(error.field, "coord_worker_upstream");
}

#[test]
fn a_downstream_arm_this_build_does_not_know_is_refused_by_its_field_number() {
    // 11 is `reserved` in CoordWorkerDown: a peer that still sends it is from
    // a build this one has to be able to say so about.
    let error =
        decode_downstream(&unknown_arm_frame(11)).expect_err("field 11 is reserved, not an arm");
    assert!(
        error.reason.contains("11"),
        "the refusal must name the field it could not place, got: {}",
        error.reason
    );
    assert_eq!(error.field, "coord_worker_downstream");
}

#[test]
fn a_frame_with_no_arm_at_all_is_refused_rather_than_read_as_empty() {
    // Zero bytes is a well-formed protobuf message with no fields. Reading it
    // as "a frame with nothing in it" is how a dropped arm becomes a session
    // that looks live.
    assert!(decode_upstream(&[]).is_err());
    assert!(decode_downstream(&[]).is_err());
}

/// A `WInputResult` whose phase is the proto's UNSPECIFIED, which proto3
/// encodes by omitting the field: request_id "r", the session uuid, input_seq
/// 1, status REJECTED (2), and no phase at all.
fn input_result_without_a_phase() -> Vec<u8> {
    let mut inner = Vec::new();
    inner.extend_from_slice(&[0x0a, 0x01, b'r']);
    inner.push(0x12);
    inner.push(SESSION.len() as u8);
    inner.extend_from_slice(SESSION.as_bytes());
    inner.extend_from_slice(&[0x18, 0x01, 0x20, 0x02]);
    let mut frame = vec![0x82, 0x01, inner.len() as u8];
    frame.extend_from_slice(&inner);
    frame
}

#[test]
fn an_input_result_with_no_write_phase_is_refused() {
    // Only a phase strictly before the keeper write can promise no mutation
    // occurred, so it is the sole basis on which the coordinator rolls
    // provisional state back. An omitted phase proves nothing, and decoding it
    // as anything retry-safe would let the browser re-send a write that landed.
    let error = decode_upstream(&input_result_without_a_phase())
        .expect_err("an omitted phase must not decode");
    assert!(
        error.reason.contains("phase"),
        "the refusal must name the phase, got: {}",
        error.reason
    );
}

#[test]
fn a_pong_stamped_before_the_epoch_is_refused_rather_than_wrapped() {
    // The wire field is a `uint64`. A negative timestamp cannot be carried, and
    // wrapping it would move the keepalive centuries into the future instead of
    // saying the value was impossible.
    let frame = CoordWorkerUpstream::Pong {
        ts: -1,
        trace_id: None,
    };
    let error = encode_upstream(&frame).expect_err("a negative pong must not encode");
    assert!(
        error.reason.contains("non-negative"),
        "the refusal must say why, got: {}",
        error.reason
    );
}
