#![cfg(unix)]
//! A real agent process, found by the real `ps` scan, judged by the pinned
//! manifests, reaches the coordinator link as one identified agent-status
//! frame — the production path of v2 `main.ts:220-237` (registry publish →
//! `coordLink.sendAgentStatus`). Also pins `read_visible_screen` over a real
//! terminal core: a wide glyph's continuation cell contributes nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_status_support;
mod link_downstream_support;

use std::sync::Arc;
use std::time::Duration;

use agent_status_support::{
    DetectorParts, SESSION_ID, ScriptedSessions, TestClock, detector_over, test_environment,
};
use link_downstream_support::live::{LiveLink, go_live, next_frame};
use link_downstream_support::{Fakes, OwnerMode};
use roost_observability::clock::EventClock;
use roost_protocol::wire::agent_status::AgentStatusSource;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream as Up;
use roost_term::{RioCore, TerminalCore};
use roost_worker::agents::detector::sessions::{AgentSessionSource, read_visible_screen};
use roost_worker::agents::process_scan::{AgentProcessScan, AgentProcessScanner};
use roost_worker::agents::process_snapshot::PsSnapshotReader;
use roost_worker::agents::registry::{
    AgentStatusRegistry, AgentStatusRegistryOptions, INTEGRATION_LEASE_MS,
};
use roost_worker::agents::status_stack::UplinkAgentStatusPublisher;

#[tokio::test(flavor = "multi_thread")]
async fn a_detected_agent_process_reaches_the_coordinator_link() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let live = LiveLink::start(&fakes, None).await;
    let mut socket = live.accept().await;
    go_live(&mut socket, Vec::new()).await;

    // `exec -a` names the process `codex` in its argv, as an installed agent
    // binary would be; the session's child IS the agent here.
    let mut agent = tokio::process::Command::new("bash")
        .args(["-c", "exec -a codex sleep 30"])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let agent_pid = agent.id().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let clock = TestClock::at(1_000);
    let registry = AgentStatusRegistry::new(AgentStatusRegistryOptions {
        publish: Arc::new(UplinkAgentStatusPublisher::new(live.uplink.clone())),
        clock: Arc::clone(&clock) as Arc<dyn EventClock>,
        lease_ms: INTEGRATION_LEASE_MS,
    })
    .unwrap();
    let sessions = Arc::new(ScriptedSessions::default());
    sessions.add_with_child("", agent_pid);
    let scanner = AgentProcessScanner::new(
        Arc::new(PsSnapshotReader::default()),
        Duration::ZERO,
        tokio::runtime::Handle::current(),
    );
    let detector = detector_over(DetectorParts {
        sessions: sessions as Arc<dyn AgentSessionSource>,
        scanner: Arc::new(scanner) as Arc<dyn AgentProcessScan>,
        registry: Arc::clone(&registry),
        clock: Arc::clone(&clock),
        environment: test_environment(),
    });
    // The acquisition grace withholds the first evaluation; the second agrees.
    detector.scan_now().await;
    clock.advance(200);
    detector.scan_now().await;

    let Up::AgentStatus(frame) = next_frame(&mut socket).await else {
        panic!("the first frame after going live is the agent status");
    };
    let status = frame.status;
    assert!(status.active);
    assert_eq!(status.common.session_id.as_str(), SESSION_ID);
    assert_eq!(status.common.agent_id.as_str(), "codex");
    assert_eq!(status.common.source, Some(AgentStatusSource::Screen));
    assert_eq!(
        status.common.status_epoch.as_ref(),
        Some(registry.status_epoch())
    );
    assert!(status.common.occupant_id.is_some());

    detector.dispose();
    registry.dispose();
    agent.kill().await.unwrap();
    live.stop().await;
}

#[test]
fn the_visible_screen_reads_a_wide_glyph_as_one_character() {
    let mut core = RioCore::new(8, 2);
    core.write("中文 ok\r\nnext".as_bytes());
    assert_eq!(read_visible_screen(&core), "中文 ok\nnext");
}
