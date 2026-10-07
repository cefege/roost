//! Local agent-report socket authentication and admission. The caller supplies
//! only session-authorized state; a fresh detector identity and the server's
//! serialized monotonic sequence are the registry input. Mirrors v2
//! `apps/worker/tests/agents/agent-status-report-server.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

#[path = "agent_report_support/mod.rs"]
mod agent_report_support;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use agent_report_support::{
    Detector, PeerPid, Reports, SESSION, capability, environment, fixed_peer, report_line, request,
    start,
};
use roost_observability::clock::SystemClock;
use roost_protocol::wire::agent_status::{AgentRuntimeState, AgentStatusSource, AgentStatusUpdate};
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::registry::{
    AgentStatusPublisher, AgentStatusRegistry, AgentStatusRegistryOptions,
};
use roost_worker::agents::report_protocol::REPORTED_STATE_UNKNOWN_REASON;
use scratch::Scratch;
use serde_json::{Value, json};

#[derive(Default)]
struct Published(std::sync::Mutex<Vec<AgentStatusUpdate>>);

impl AgentStatusPublisher for Published {
    fn publish(&self, status: AgentStatusUpdate) {
        self.0.lock().unwrap().push(status);
    }
}

fn error_of(answer: &Value) -> Option<&str> {
    assert_eq!(answer["ok"], json!(false), "{answer}");
    answer["error"].as_str()
}

#[tokio::test]
async fn accepts_state_with_fresh_worker_derived_identity_and_ordering() {
    let scratch = Scratch::new("agent-report-accept");
    let environment = environment(&scratch);
    let published = Arc::new(Published::default());
    let registry = AgentStatusRegistry::new(AgentStatusRegistryOptions {
        publish: Arc::clone(&published) as Arc<dyn AgentStatusPublisher>,
        clock: Arc::new(SystemClock),
        lease_ms: 30_000,
    })
    .expect("a registry");
    let detector = Detector::knowing(BuiltinAgentId::Omp, 42);
    let peer = Arc::new(PeerPid::default());
    peer.set(42);
    let reports = Arc::new(Reports {
        forward: Some(registry),
        ..Reports::default()
    });
    let server = start(
        &environment,
        Arc::clone(&detector),
        Arc::clone(&reports),
        Some(fixed_peer(&peer)),
    );

    let mode = std::fs::metadata(server.path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "the socket is this user's alone");
    assert_eq!(
        request(server.path(), &report_line(&environment, json!({}))).await,
        json!({ "ok": true })
    );
    detector.set(Some((BuiltinAgentId::Pi, 84)));
    peer.set(84);
    let blocked = report_line(&environment, json!({ "state": "blocked" }));
    assert_eq!(
        request(server.path(), &blocked).await,
        json!({ "ok": true })
    );

    let received = reports.received.lock().unwrap().clone();
    let identities: Vec<_> = received
        .iter()
        .map(|report| (report.agent_id, report.process_id))
        .collect();
    assert_eq!(
        identities,
        [(BuiltinAgentId::Omp, 42), (BuiltinAgentId::Pi, 84)]
    );
    assert!(
        received[1].seq > received[0].seq,
        "the worker's sequence orders reports"
    );
    let last = published
        .0
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the registry published");
    assert_eq!(last.common.session_id.as_str(), SESSION);
    assert_eq!(last.common.agent_id.as_str(), "pi");
    assert_eq!(last.common.state, AgentRuntimeState::Blocked);
    assert!(last.active);
    assert_eq!(last.common.source, Some(AgentStatusSource::Integration));
    server.close().await;
}

#[tokio::test]
async fn rejects_unavailable_identity_caller_selected_identity_and_malformed_state() {
    let scratch = Scratch::new("agent-report-reject");
    let environment = environment(&scratch);
    let detector = Arc::new(Detector::default());
    let peer = Arc::new(PeerPid::default());
    peer.set(42);
    let reports = Arc::new(Reports::default());
    let server = start(
        &environment,
        Arc::clone(&detector),
        Arc::clone(&reports),
        Some(fixed_peer(&peer)),
    );
    let path = server.path().to_path_buf();

    assert_eq!(
        error_of(&request(&path, "not json\n").await),
        Some("invalid_json")
    );
    let mismatch = Some("reporter_identity_mismatch");
    assert_eq!(
        error_of(&request(&path, &report_line(&environment, json!({}))).await),
        mismatch
    );
    let other = report_line(
        &environment,
        json!({ "session_id": "22222222-2222-4222-8222-222222222222" }),
    );
    assert_eq!(error_of(&request(&path, &other).await), mismatch);
    detector.set(Some((BuiltinAgentId::Omp, 42)));
    peer.set(7);
    assert_eq!(
        error_of(&request(&path, &report_line(&environment, json!({}))).await),
        mismatch
    );
    peer.set(42);
    let selected = report_line(
        &environment,
        json!({ "pid": 7, "agent": "pi", "seq": 9_007_199_254_740_991_u64 }),
    );
    assert_eq!(
        error_of(&request(&path, &selected).await),
        Some("invalid_request")
    );
    let long = report_line(&environment, json!({ "message": "x".repeat(513) }));
    assert_eq!(
        error_of(&request(&path, &long).await),
        Some("invalid_request")
    );
    let unknown = request(
        &path,
        &report_line(&environment, json!({ "state": "unknown" })),
    )
    .await;
    assert_eq!(error_of(&unknown), Some("invalid_request"));
    assert_eq!(unknown["detail"], json!(REPORTED_STATE_UNKNOWN_REASON));
    assert!(
        reports.received.lock().unwrap().is_empty(),
        "nothing reached the registry"
    );
    server.close().await;
}

#[tokio::test]
async fn refuses_a_capability_minted_for_another_session() {
    let scratch = Scratch::new("agent-report-capability");
    let environment = environment(&scratch);
    let peer = Arc::new(PeerPid::default());
    peer.set(42);
    let reports = Arc::new(Reports::default());
    let detector = Detector::knowing(BuiltinAgentId::Omp, 42);
    let server = start(
        &environment,
        detector,
        Arc::clone(&reports),
        Some(fixed_peer(&peer)),
    );

    let mut line: Value =
        serde_json::from_str(report_line(&environment, json!({})).trim()).unwrap();
    line["capability"] = json!(capability(
        &environment,
        agent_report_support::OTHER_SESSION
    ));
    let answer = request(server.path(), &format!("{line}\n")).await;
    assert_eq!(error_of(&answer), Some("authentication_failed"));
    assert!(reports.received.lock().unwrap().is_empty());
    server.close().await;
}

#[tokio::test]
async fn caps_oversized_local_input() {
    let scratch = Scratch::new("agent-report-cap");
    let environment = environment(&scratch);
    let peer = Arc::new(PeerPid::default());
    peer.set(42);
    let detector = Detector::knowing(BuiltinAgentId::Omp, 42);
    let server = start(
        &environment,
        detector,
        Arc::default(),
        Some(fixed_peer(&peer)),
    );
    let answer = request(server.path(), &format!("{}\n", "x".repeat(33_000))).await;
    assert_eq!(error_of(&answer), Some("request_too_large"));
    server.close().await;
}

/// A second line is refused before the first is looked at, and the first
/// still takes effect: its answer is lost, its admission is not.
#[tokio::test]
async fn a_second_request_line_is_refused_while_the_first_still_lands() {
    let scratch = Scratch::new("agent-report-two-lines");
    let environment = environment(&scratch);
    let peer = Arc::new(PeerPid::default());
    peer.set(42);
    let reports = Arc::new(Reports::default());
    let detector = Detector::knowing(BuiltinAgentId::Omp, 42);
    let server = start(
        &environment,
        detector,
        Arc::clone(&reports),
        Some(fixed_peer(&peer)),
    );
    let line = report_line(&environment, json!({}));
    let answer = request(server.path(), &format!("{line}{line}")).await;
    assert_eq!(error_of(&answer), Some("too_many_requests"));
    server.close().await;
    assert_eq!(reports.received.lock().unwrap().len(), 1);
}

/// An `agent.reference` line from an OMP extension installed by an older
/// worker is an unknown method: refused, never admitted.
#[tokio::test]
async fn an_agent_reference_line_is_refused_as_an_unknown_method() {
    let scratch = Scratch::new("agent-report-reference");
    let environment = environment(&scratch);
    let peer = Arc::new(PeerPid::default());
    peer.set(42);
    let detector = Detector::knowing(BuiltinAgentId::Omp, 42);
    let reports = Arc::new(Reports::default());
    let server = start(
        &environment,
        detector,
        Arc::clone(&reports),
        Some(fixed_peer(&peer)),
    );
    let line = json!({
        "version": 1,
        "capability": capability(&environment, SESSION),
        "method": "agent.reference",
        "params": { "session_id": SESSION, "reference": { "kind": "id", "value": "x" } },
    });
    let answer = request(server.path(), &format!("{line}\n")).await;
    assert_eq!(error_of(&answer), Some("invalid_request"));
    server.close().await;
    assert!(reports.received.lock().unwrap().is_empty());
}
