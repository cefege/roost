//! The generated wire code, exercised end to end.
//!
//! The build script rewrites every field's `json_name` to protobuf's canonical
//! lowerCamelCase form, because `protox` only records a `json_name` the `.proto`
//! spells explicitly. These tests are the guard for that rewrite: a Connect
//! JSON client, a `curl` body, and every committed conformance vector send
//! `fgRgb`, not `fg_rgb`, and a regression here is a browser that silently
//! stops parsing terminal frames.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use roost_proto::buffa::{DecodeOptions, Message};
use roost_proto::{
    OpenedEvt, PbCellGridChunk, PbCellGridFrame, PbCellRow, PbCellSpan, SessionEventProto,
};

fn sample_span() -> PbCellSpan {
    PbCellSpan {
        text: "r0".to_owned(),
        fg: 256,
        bg: 256,
        columns: 2,
        ..Default::default()
    }
}

fn sample_frame() -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: "session".to_owned(),
        cols: 8,
        rows: 4,
        full: true,
        viewport_rows: vec![PbCellRow {
            index: 0,
            spans: vec![sample_span()],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn sample_chunk() -> PbCellGridChunk {
    PbCellGridChunk {
        snapshot_id: "00000000-0000-4000-8000-000000000011".to_owned(),
        chunk_index: 0,
        chunk_count: 1,
        part: roost_proto::buffa::MessageField::some(sample_frame()),
        ..Default::default()
    }
}

#[test]
fn a_message_survives_a_binary_round_trip() {
    let encoded = sample_chunk().encode_to_vec();
    let decoded = DecodeOptions::new()
        .decode_from_slice::<PbCellGridChunk>(&encoded)
        .expect("the encoder and decoder come from the same descriptor");
    assert_eq!(decoded, sample_chunk());
}

#[test]
fn a_message_decodes_from_canonical_protobuf_json() {
    // The exact spelling a Connect JSON client and `protocol/conformance` use.
    let json = r#"{
        "snapshotId": "00000000-0000-4000-8000-000000000011",
        "chunkCount": 1,
        "part": {
            "sessionId": "session",
            "cols": 8,
            "rows": 4,
            "full": true,
            "viewportRows": [{ "index": 0, "spans": [
                { "text": "r0", "fg": "256", "bg": "256", "columns": 2 }
            ]}]
        }
    }"#;
    let decoded: PbCellGridChunk =
        serde_json::from_str(json).expect("canonical protobuf JSON must decode");
    assert_eq!(decoded, sample_chunk());
}

#[test]
fn a_uint64_event_id_survives_the_json_encoding() {
    // 2^53 + 1: the value a JavaScript number would round away. A field that
    // round-trips as 9007199254740992 is a dropped event in the log.
    let event_id = 9_007_199_254_740_993u64;
    let event = SessionEventProto {
        event_id,
        kind: Some(
            OpenedEvt {
                session_id: "00000000-0000-4000-8000-000000000101".to_owned(),
                worker_fp: "a".repeat(64),
                channel: 1,
                session_kind: "shell".to_owned(),
                cwd: "/repo".to_owned(),
                ts: 1,
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    };
    let json = serde_json::to_string(&event).expect("the event encodes");
    assert!(json.contains("9007199254740993"), "got {json}");
    let decoded: SessionEventProto = serde_json::from_str(&json).expect("the event decodes");
    assert_eq!(decoded.event_id, event_id);
}
