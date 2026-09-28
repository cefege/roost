//! The `window.__smoke` member table and argument grammar the unmodified
//! Playwright oracle calls, and the `state()` record its fixture waits on
//! (`state().workers[fp]`). Pins `smoke::{call, state_snapshot}` (v2
//! `apps/web/src/smoke/smokeTypes.ts:118-287`, `smokeRuntimeControls.ts:59-66`).
#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_client_core::store::{PairRequest, Worker, WorkerFp, WorkerOs};
use roost_client_core::ClientCore;
use roost_web::smoke::call::{
    Answer, SMOKE_METHODS, SmokeCall, TimingKind, UNPORTED_METHODS, parse_call,
};
use roost_web::smoke::state_snapshot::state_json;
use serde_json::{Value, json};

const FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn call(name: &str, args: &[Value]) -> Result<SmokeCall, String> {
    parse_call(name, args)
}

#[test]
fn every_member_is_named_once_and_every_refusal_names_a_member() {
    let names: BTreeSet<&str> = SMOKE_METHODS.iter().map(|(name, _)| *name).collect();
    assert_eq!(names.len(), SMOKE_METHODS.len());
    for (unported, refusal) in UNPORTED_METHODS {
        assert!(names.contains(unported), "{unported}");
        assert_eq!(call(unported, &[]), Ok(SmokeCall::Unported { refusal }));
        assert!(refusal.starts_with("U-2 "), "a refusal names its slice: {refusal}");
    }
    assert!(call("nope", &[]).unwrap_err().contains("no member nope"));
}

#[test]
fn members_the_oracle_awaits_answer_with_a_promise() {
    let promised: BTreeSet<&str> = SMOKE_METHODS
        .iter()
        .filter(|(_, answer)| *answer == Answer::Promise)
        .map(|(name, _)| *name)
        .collect();
    for awaited in ["input", "waitForPaintedMarker", "spawnShell", "runFlow", "cleanupCreated", "retainedMarkerScan", "beginTerminalTiming"] {
        assert!(promised.contains(awaited), "{awaited}");
    }
    for plain in ["state", "viewportText", "markerScan", "cellFrameCount", "navigate", "dropNextCellFrame"] {
        assert!(!promised.contains(plain), "{plain}");
    }
}

#[test]
fn arguments_parse_into_typed_calls_with_v2s_defaults() {
    assert_eq!(
        call("input", &[json!("s-1"), json!("ls\r")]),
        Ok(SmokeCall::Input { session_id: "s-1".into(), text: "ls\r".into() })
    );
    assert_eq!(
        call("waitForPaintedMarker", &[json!("s-1"), json!("M1")]),
        Ok(SmokeCall::WaitForPaintedMarker { session_id: "s-1".into(), marker: "M1".into(), timeout_ms: 30_000.0 })
    );
    assert_eq!(
        call("spawnShell", &[json!(FP), json!("/tmp"), Value::Null]),
        Ok(SmokeCall::SpawnShell { worker_fp: FP.into(), folder: "/tmp".into(), session_id: None })
    );
    assert_eq!(call("runFlow", &[]), Ok(SmokeCall::RunFlow { worker_fp: None }));
    assert_eq!(
        call("runFlow", &[json!({ "workerFp": FP })]),
        Ok(SmokeCall::RunFlow { worker_fp: Some(FP.into()) })
    );
    assert_eq!(
        call("beginTerminalTiming", &[json!("trusted_key"), json!("s-1")]),
        Ok(SmokeCall::BeginTiming { kind: TimingKind::TrustedKey, session_id: Some("s-1".into()) })
    );
    assert_eq!(
        call("attachmentProbe", &[json!("s-1"), json!("ab"), json!(12)]),
        Ok(SmokeCall::AttachmentProbe { session_id: "s-1".into(), sha256: "ab".into(), size: 12, filename: "probe.bin".into() })
    );
    assert_eq!(
        call("cellFrameCount", &[json!("s-1")]),
        Ok(SmokeCall::Session { method: "cellFrameCount", session_id: "s-1".into() })
    );
    let SmokeCall::RunRenderStress(options) = call(
        "runRenderStress",
        &[json!({ "sessionId": "s-1", "prefix": "M", "screen": "alt", "iterations": 6 })],
    )
    .unwrap() else {
        panic!("runRenderStress parses into its options");
    };
    assert!(!options.main_screen);
    assert_eq!(options.iterations, 6);
}

#[test]
fn a_wrong_argument_is_refused_with_the_member_and_position() {
    assert_eq!(
        call("viewportText", &[json!(7)]).unwrap_err(),
        "__smoke.viewportText: argument 1 must be a string"
    );
    assert!(call("hasPaintedScrollbackRange", &[json!("s"), json!(-1), json!(3)]).is_err());
    assert!(call("beginTerminalTiming", &[json!("slow")]).unwrap_err().contains("unknown terminal timing kind"));
    let cursor = call("waitForPaintedCursor", &[json!("s"), json!({ "row": -1 })]).unwrap_err();
    assert!(cursor.starts_with("invalid expected cursor coordinates"), "{cursor}");
    assert!(call("runRenderStress", &[json!({ "sessionId": "s", "prefix": "M", "screen": "side", "iterations": 1 })]).is_err());
}

fn worker(last_seen_ms: i64) -> Worker {
    Worker {
        fp: WorkerFp::try_from(FP.to_owned()).expect("a fingerprint"),
        label: "workstation".to_owned(),
        os: WorkerOs::Linux,
        host_identity: None,
        git_sha: None,
        host_metrics: None,
        registered_at_ms: 1,
        last_seen_ms,
        reachable_addr: None,
        keeper_runtime: None,
        terminal_core_capacity: None,
    }
}

#[test]
fn state_publishes_workers_keyed_by_fingerprint_and_pair_requests_in_v2s_shape() {
    let mut core = ClientCore::in_memory("tab-1");
    core.store_mut().workers.insert(FP.to_owned(), worker(42));
    core.store_mut().pair_requests.insert(
        "e-1".to_owned(),
        PairRequest {
            ephemeral_id: "e-1".into(),
            label: "phone".into(),
            created_at_ms: 5,
            user_agent: "ua".into(),
            client_browser: "Firefox".into(),
            client_os: "Android".into(),
            client_device_type: "mobile".into(),
            source_ip: "10.0.0.2".into(),
            country_code: "NZ".into(),
            region: String::new(),
            city: String::new(),
            edge_identity_provider: String::new(),
            edge_identity: String::new(),
            edge_identity_verified: false,
            expires_at_ms: 99,
        },
    );
    let state = state_json(core.store());
    assert_eq!(state["workers"][FP]["fp"], FP);
    assert_eq!(state["workers"][FP]["last_seen_ms"], 42);
    assert_eq!(state["sessions"], json!({}));
    assert_eq!(state["workspaces"], json!({}));
    let request = &state["pair_requests"]["e-1"];
    assert_eq!((request["clientBrowser"].clone(), request["expiresAtMs"].clone()), (json!("Firefox"), json!(99)));
    assert_eq!(request["ephemeral_id"], "e-1");
}
