#![cfg(unix)]
//! The acceptance case for terminal capture: a REAL shell on a REAL keeper,
//! through the production session layer (`session_stack::build`, which builds
//! the one recorder and attaches its tap to the one emitter), writes output
//! the recorder retains, and a CAPTURE freezes it into a gzip bundle that
//! carries that terminal content. No v2 counterpart: v2's suites drive a fake
//! keeper; this is the end-to-end proof the port's wiring is live.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod keeper_pool_support;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use keeper_pool_support::KeeperFixture;
use roost_protocol::terminal_capture::TerminalCaptureStatus;
use roost_protocol::wire::brand::{SessionId, WorkerFp};
use roost_worker::agents::environment::AgentReportSite;
use roost_worker::browser_commands::diagnostics::{
    CaptureAction, CaptureCommand, DiagnosticReports,
};
use roost_worker::browser_commands::session_lifecycle::SessionLifecycle;
use roost_worker::event_store::Journal;
use roost_worker::runtime::session_stack;
use serde_json::Value;
use tokio::io::AsyncReadExt as _;

const FINGERPRINT: &str = "000000000000000000000000000000000000000000000000000000000000ca97";
const SESSION: &str = "00000000-0000-4000-8000-0000000ca970";
const RECORDING: &str = "cccccccc-0000-4000-8000-0000000ca971";
/// The shell computes the marker, so the echoed command line alone
/// (`$((6*7))`) can never satisfy the assertion.
const MARKER: &str = "CAPTURE-MARKER-42";

fn step(action: CaptureAction, capture_id: &str) -> CaptureCommand {
    CaptureCommand {
        action,
        session_id: SessionId::try_from(SESSION).unwrap(),
        recording_id: RECORDING.to_owned(),
        capture_id: capture_id.to_owned(),
        reason: "manual".to_owned(),
        browser_evidence_json: String::new(),
        coordinator_evidence_json: String::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capture_of_a_real_session_carries_its_terminal_content() {
    let fixture = KeeperFixture::start();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("roost-capture-real-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    let outbox = Arc::new(
        Journal::open(&root.join("session-events.sqlite"))
            .await
            .unwrap(),
    );
    let stack = session_stack::build(
        WorkerFp::try_from(FINGERPRINT).unwrap(),
        fixture.pool(),
        outbox,
        &root,
        &root,
        FINGERPRINT.to_owned(),
        "capture-real-epoch",
        &AgentReportSite {
            data_dir: root.clone(),
            configured: None,
        },
    )
    .expect("this host can build a session layer");
    let session_id = SessionId::try_from(SESSION).unwrap();
    stack
        .manager
        .spawn_shell(
            root.display().to_string(),
            Some(80),
            Some(24),
            Some(session_id.clone()),
        )
        .await
        .expect("a real shell opens on the keeper");

    let armed = stack.capture.start_recording(step(
        CaptureAction::Start,
        "eeeeeeee-0000-4000-8000-0000000ca972",
    ));
    assert_eq!(armed.status, TerminalCaptureStatus::Recording, "{armed:?}");
    let _ = stack
        .manager
        .write_worker_owned_input(&session_id, b"echo CAPTURE-MARKER-$((6*7))\n".to_vec())
        .await;

    // The output arrives on the keeper's dispatch thread; wait for the ring.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let retained = stack
            .table
            .with_record(&session_id, |record| record.scrollback.to_vec())
            .unwrap_or_default();
        if String::from_utf8_lossy(&retained).contains(MARKER) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never printed the marker"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let ack = stack
        .capture
        .capture(step(
            CaptureAction::Capture,
            "eeeeeeee-0000-4000-8000-0000000ca973",
        ))
        .await;
    assert_eq!(ack.error, None, "{ack:?}");
    let path = ack.path.expect("a written bundle names its file");
    let compressed = tokio::fs::read(&path).await.unwrap();
    let mut json = String::new();
    async_compression::tokio::bufread::GzipDecoder::new(compressed.as_slice())
        .read_to_string(&mut json)
        .await
        .unwrap();
    let bundle: Value = serde_json::from_str(&json).unwrap();

    // The armed recorder retained the PTY bytes with their exact offsets.
    let raw: Vec<u8> = bundle["worker"]["raw"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|record| {
            base64::engine::general_purpose::STANDARD
                .decode(record["base64"].as_str().unwrap())
                .unwrap()
        })
        .collect();
    assert!(
        String::from_utf8_lossy(&raw).contains(MARKER),
        "the raw chain carries the shell's output"
    );
    assert_eq!(
        bundle["worker"]["geometry"],
        serde_json::json!({ "cols": 80, "rows": 24 })
    );
    assert_eq!(bundle["session_id"], serde_json::json!(SESSION));

    let _ = stack.manager.kill(session_id).await;
    let _ = std::fs::remove_dir_all(&root);
}
