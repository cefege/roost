//! The link codec is a DELEGATION, and this is the only test that can tell.
//!
//! `ProtoLinkWire` replaced a stub that refused every frame, so it is a real
//! implementation on a path that used to fail — and it had no test at all. A
//! round-trip test through `ProtoLinkWire` alone would NOT catch the defect its
//! own module header names: a second mapping written against the union's field
//! numbers is perfectly self-consistent, so encode-then-decode through the
//! second mapping passes while disagreeing with the wire on every frame that
//! matters. The only assertion that has teeth compares against the shared
//! mapping directly.
//!
//! The other thing worth pinning is that `hello` CAN carry capabilities and a
//! process epoch at all. An empty repeated field encodes to zero bytes, so a
//! hello sent with an empty capability list is byte-identical to one sent by a
//! build that had no such field — which is a real parity gap in what the link
//! currently sends, and is invisible in a test that only checks a round trip.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::Value;

use roost_protocol::proto_adapters::coord_worker_proto;
use roost_protocol::wire::brand::{TraceId, WorkerFp};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_worker::runtime::link_wire::{LinkWire, ProtoLinkWire, WireError};

/// NOT a uuid, unlike the trace id below it. `WorkerFp` is the SHA-256 hex of a
/// worker's ed25519 pubkey — 64 lowercase hex characters and nothing else, with
/// no `sha256:` prefix (`roost_protocol::wire::WorkerFp::check`). Sitting next
/// to a uuid-looking string it was read as one, and every frame in this file
/// failed in `worker_fp()` before reaching the codec under test.
fn worker_fp() -> WorkerFp {
    WorkerFp::try_from("000000000000000000000000000000000000000000000000000000000000f00d")
        .expect("a 64-character lowercase hex digest is a worker fp")
}

fn trace_id() -> TraceId {
    TraceId::try_from("a1b2c3d4e5f60718").expect("eight hex characters is a trace id")
}

/// The frames the worker's own lifecycle actually sends, one per shape rather
/// than one per variant: the mapping has 19 upstream arms and this is not a
/// substitute for testing all of them, it is the sample that catches a
/// hand-rolled second definition disagreeing about field numbers.
fn upstream_frames() -> Vec<CoordWorkerUpstream> {
    vec![
        CoordWorkerUpstream::Hello {
            worker_fp: worker_fp(),
            version: "3.0.0".to_owned(),
            capabilities: vec!["terminal-metadata-v1".to_owned()],
            process_epoch: "epoch-1".to_owned(),
            trace_id: Some(trace_id()),
        },
        CoordWorkerUpstream::Pong {
            ts: 1_700_000_000_000,
            trace_id: Some(trace_id()),
        },
        CoordWorkerUpstream::RpcOk {
            request_id: "req-1".to_owned(),
            data: Value::String("ok".to_owned()),
            trace_id: None,
        },
        CoordWorkerUpstream::RpcError {
            request_id: "req-2".to_owned(),
            message: "no such session".to_owned(),
            trace_id: None,
        },
    ]
}

/// The wire's bytes are the shared mapping's bytes, for every arm sampled.
///
/// This is the assertion that fails if someone writes a second mapping here. It
/// compares two independent calls to the SAME function, which is only
/// meaningful because the thing under test is supposed to BE that function:
/// a parallel implementation would produce different bytes and this catches it
/// on the first frame rather than on the first dropped terminal.
#[test]
fn the_encoded_bytes_are_the_shared_mappings_bytes() {
    for frame in upstream_frames() {
        let through_wire = ProtoLinkWire
            .encode_upstream(&frame)
            .unwrap_or_else(|error| panic!("the shared mapping encodes this arm: {error}"));
        let through_mapping = coord_worker_proto::encode_upstream(&frame)
            .unwrap_or_else(|error| panic!("the shared mapping encodes this arm: {error}"));

        assert_eq!(
            through_wire, through_mapping,
            "the link wire is not the shared mapping for this arm; a second definition \
             of the wire agrees with the first right up until it does not, and on this \
             socket that is a terminal frame the coordinator reports as a live session \
             that has stopped painting"
        );
        assert!(
            !through_wire.is_empty(),
            "an arm that encodes to zero bytes is indistinguishable from a build with \
             no such field"
        );
    }
}

/// A hello CAN carry capabilities; a build that has not started sending them
/// yet is the gap, not the wire.
///
/// `capabilities` is a repeated field and `process_epoch` is an optional one,
/// so a hello with both empty is byte-identical to a hello from a build that
/// had neither. That is exactly why the difference has to be asserted here
/// rather than discovered on the wire: if the field were not carried at all,
/// the coordinator would negotiate view ownership from nothing.
#[test]
fn a_hello_with_capabilities_differs_on_the_wire_from_one_without() {
    let bare = ProtoLinkWire
        .encode_upstream(&CoordWorkerUpstream::Hello {
            worker_fp: worker_fp(),
            version: "3.0.0".to_owned(),
            capabilities: Vec::new(),
            process_epoch: String::new(),
            trace_id: None,
        })
        .expect("a hello encodes");
    let loaded = ProtoLinkWire
        .encode_upstream(&CoordWorkerUpstream::Hello {
            worker_fp: worker_fp(),
            version: "3.0.0".to_owned(),
            capabilities: vec![
                "terminal-metadata-v1".to_owned(),
                "terminal-view-owner-v1".to_owned(),
            ],
            process_epoch: "epoch-7".to_owned(),
            trace_id: Some(trace_id()),
        })
        .expect("a hello encodes");

    assert_ne!(
        bare, loaded,
        "capabilities and a process epoch must reach the wire: an empty repeated field \
         encodes to nothing, so a hello that omits them is byte-identical to a build \
         that never had them"
    );
}

/// A frame that will not decode says so, and says WHY, in the shared mapping's
/// own words.
///
/// The variant is pinned rather than asserted with `is_err()`: `Unencodable` and
/// `Undecodable` are opposite claims about which direction failed, and a link
/// loop that logged the wrong one would send an operator looking in the wrong
/// direction.
#[test]
fn a_frame_that_will_not_decode_carries_the_shared_mappings_diagnosis() {
    let garbage = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];

    let refused = ProtoLinkWire
        .decode_downstream(&garbage)
        .expect_err("eight bytes of 0xff is not a downstream frame");

    let shared_reason = coord_worker_proto::decode_downstream(&garbage)
        .expect_err("the shared mapping refuses the same bytes")
        .to_string();

    match refused {
        WireError::Undecodable { reason } => assert_eq!(
            reason, shared_reason,
            "the diagnosis must be the shared mapping's, or an operator is told why \
             this build failed rather than why the wire did"
        ),
        other => panic!("expected Undecodable for a downstream frame, got {other:?}"),
    }
}
