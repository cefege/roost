//! What a browser-to-worker control frame IS: every kind the contract lists,
//! its canonical JSON, and the strictness a kind is decoded with.
//!
//! A frame is the boundary between two builds that can be deployed separately,
//! so each case here is either a frame a live browser emits or a refusal that
//! keeps a half-understood frame from reaching a PTY. What a frame is allowed
//! to ask for is the other half, in `control_frame_bounds.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;

use roost_protocol::terminal_search::GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS;
use roost_protocol::wire::control::ClientControlFrame;

use support::{CONTROL_SESSION, canonical_frames, frame_of};

#[test]
fn every_control_frame_kind_parses_its_canonical_json() {
    for (kind, value) in canonical_frames() {
        let frame = ClientControlFrame::parse(value)
            .unwrap_or_else(|error| panic!("{kind} must parse: {error}"));
        assert_eq!(frame.kind(), kind);
    }
}

#[test]
fn every_control_frame_kind_is_refused_without_its_discriminant() {
    for (kind, value) in canonical_frames() {
        let mut without_kind = value;
        without_kind
            .as_object_mut()
            .expect("a frame is a JSON object")
            .remove("kind");
        assert!(
            ClientControlFrame::parse(without_kind).is_err(),
            "{kind} was admitted without its discriminant"
        );
    }
}

#[test]
fn every_canonical_frame_survives_its_own_round_trip() {
    for (kind, value) in canonical_frames() {
        let frame = ClientControlFrame::parse(value).expect("a canonical frame parses");
        let encoded = serde_json::to_value(&frame).expect("a frame serializes");
        assert_eq!(
            ClientControlFrame::parse(encoded),
            Ok(frame),
            "{kind} did not survive a round trip"
        );
    }
}

#[test]
fn a_frame_kind_this_build_retired_is_refused_rather_than_ignored() {
    // The cross-worker transfer commands are gone from the contract. A peer
    // still sending one is told nothing happened, never silently acked.
    assert!(
        ClientControlFrame::parse(json!({
            "kind": "start-transfer",
            "job_id": "job",
            "src_path": "/source",
            "dst_host": "worker.example",
            "dst_path": "/destination",
        }))
        .is_err()
    );
    assert!(
        ClientControlFrame::parse(json!({ "kind": "attach-to", "session_id": CONTROL_SESSION }))
            .is_err()
    );
}

#[test]
fn a_lenient_frame_tolerates_a_key_it_does_not_define() {
    // Only the batches and the capture are strict. Refusing an extra key on
    // the everyday frames would break a browser that grew a field first.
    let mut attach = frame_of("attach");
    attach["viewport"] = json!({ "cols": 80 });
    assert!(ClientControlFrame::parse(attach).is_ok());
}

#[test]
fn a_strict_batch_refuses_a_key_it_does_not_define() {
    // `regex` is a single-session search field. On a fleet-wide page it would
    // be a bounded regex over every session, which no deadline holds.
    let mut batch = frame_of("search-scrollback-batch");
    batch["regex"] = json!(false);
    let error = ClientControlFrame::parse(batch).expect_err("a strict batch refuses it");
    assert!(error.field.contains("regex"), "{error}");

    let mut cancel = frame_of("cancel-scrollback-search-batch");
    cancel["deadline_ms"] = json!(GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS);
    assert!(ClientControlFrame::parse(cancel).is_err());
}

#[test]
fn a_strict_capture_refuses_a_key_it_does_not_define() {
    let mut capture = frame_of("diag-terminal-capture");
    capture["destination"] = json!("/tmp/incident.json.gz");
    assert!(ClientControlFrame::parse(capture).is_err());
}

#[test]
fn a_batch_refuses_a_session_it_would_scan_twice() {
    let mut batch = frame_of("search-scrollback-batch");
    batch["sessions"] = json!([
        { "session_id": CONTROL_SESSION, "grid_epoch": "epoch:1" },
        { "session_id": CONTROL_SESSION, "grid_epoch": "epoch:1" },
    ]);
    assert!(ClientControlFrame::parse(batch).is_err());

    let mut cancel = frame_of("cancel-scrollback-search-batch");
    cancel["session_ids"] = json!([CONTROL_SESSION, CONTROL_SESSION]);
    assert!(ClientControlFrame::parse(cancel).is_err());
}

#[test]
fn a_respawn_without_a_geometry_spawns_at_eighty_by_twenty_four() {
    // A keeper PTY spawned at the wrong size paints to that width until the
    // first resize lands, and the wrap it leaves is in the visible buffer.
    let frame = ClientControlFrame::parse(frame_of("respawn-if-missing"))
        .expect("a respawn without a geometry is valid");
    match frame {
        ClientControlFrame::RespawnIfMissing { cols, rows, .. } => {
            assert_eq!(cols, 80);
            assert_eq!(rows, 24);
        }
        other => panic!("expected a respawn, got {other:?}"),
    }
}

#[test]
fn a_respawn_refuses_a_geometry_it_could_not_spawn() {
    for (cols, rows) in [(0, 24), (80, 0), (-1, 24), (80, -1)] {
        let mut respawn = frame_of("respawn-if-missing");
        respawn["cols"] = json!(cols);
        respawn["rows"] = json!(rows);
        assert!(
            ClientControlFrame::parse(respawn).is_err(),
            "{cols}x{rows} was admitted"
        );
    }
}
