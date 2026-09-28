//! The foreground-job fence: a prompt is admitted only while the pane's tty
//! foreground job belongs to the agent's own process subtree. The proof comes
//! from the real process scanner reading a synthetic `ps` snapshot and the real
//! status registry, so the fence runs for real. Ports
//! `apps/worker/tests/agents/agent-prompt-foreground.test.ts`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_prompt_support;
mod session_support;

use std::sync::Arc;
use std::time::Duration;

use agent_prompt_support::{CHANNEL, TestBudget, request_for};
use roost_observability::clock::EventClock;
use roost_protocol::wire::agent_status::{AgentRuntimeState, AgentStatusUpdate};
use roost_protocol::wire::brand::SessionId;
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::process_scan::{
    AgentProcessIdentity, AgentProcessScanner, SessionProcessRoot,
};
use roost_worker::agents::process_snapshot::{ProcessSnapshotReader, ScanAbort, parse_ps_snapshot};
use roost_worker::agents::process_tree::ProcessRecord;
use roost_worker::agents::prompt_control::{
    AgentProcessProver, AgentPromptControlDeps, AgentStatusProofs, NOT_FOREGROUND_REASON,
    write_agent_prompt,
};
use roost_worker::agents::registry::{
    AgentStatusPublisher, AgentStatusRegistry, AgentStatusRegistryOptions, IntegrationStatusReport,
};
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::uplink::OwnerFuture;
use session_support::{Harness, PinnedClock, SESSION, session_id};

const PANE_CHILD_PID: u32 = 100;
const AGENT_PID: u32 = 200;

/// `ps -o pid=,ppid=,pgid=,tpgid=,comm=,args=` rows: pid, ppid, pgid, tpgid, comm.
type PaneRow = (u32, u32, i32, i32, &'static str);

struct FixedPs(String);

impl ProcessSnapshotReader for FixedPs {
    fn read(&self, _abort: ScanAbort) -> OwnerFuture<Result<Vec<ProcessRecord>, String>> {
        Box::pin(std::future::ready(Ok(parse_ps_snapshot(&self.0))))
    }
}

struct Unpublished;

impl AgentStatusPublisher for Unpublished {
    fn publish(&self, _status: AgentStatusUpdate) {}
}

/// v2 `detector.reportingAgentForSession` over the scanner and one pane root.
struct PaneProver {
    scanner: Arc<AgentProcessScanner>,
    root: SessionProcessRoot,
}

impl AgentProcessProver for PaneProver {
    fn reporting_agent_for_session(
        &self,
        _session_id: &SessionId,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        let (scanner, root) = (Arc::clone(&self.scanner), self.root.clone());
        Box::pin(async move {
            scanner
                .scan_reporting_agent(&root, reporter_pid, abort)
                .await
        })
    }
}

/// A live session whose process proof is scanned out of `pane`.
async fn foreground_deps(session: &Harness, pane: &[PaneRow]) -> AgentPromptControlDeps {
    let registry = AgentStatusRegistry::new(AgentStatusRegistryOptions {
        publish: Arc::new(Unpublished),
        clock: Arc::new(PinnedClock) as Arc<dyn EventClock>,
        lease_ms: 60_000,
    })
    .unwrap();
    assert!(registry.report_integration(IntegrationStatusReport {
        session_id: session_id(SESSION),
        agent_id: BuiltinAgentId::Omp,
        process_id: AGENT_PID,
        state: AgentRuntimeState::Idle,
        message: None,
        seq: 1,
        active: true,
    }));
    let snapshot = pane
        .iter()
        .map(|(pid, ppid, pgid, tpgid, comm)| format!("{pid} {ppid} {pgid} {tpgid} {comm} {comm}"))
        .collect::<Vec<_>>()
        .join("\n");
    let scanner = Arc::new(AgentProcessScanner::new(
        Arc::new(FixedPs(snapshot)),
        Duration::ZERO,
        tokio::runtime::Handle::current(),
    ));
    let root = SessionProcessRoot {
        session_id: session_id(SESSION),
        child_pid: PANE_CHILD_PID,
    };
    let discovered = scanner.scan_agents(std::slice::from_ref(&root)).await;
    assert_eq!(
        discovered.get(&root.session_id).map(|found| found.pid),
        Some(AGENT_PID)
    );
    AgentPromptControlDeps {
        manager: Arc::clone(&session.manager),
        sessions: Arc::clone(&session.table),
        registry: registry as Arc<dyn AgentStatusProofs>,
        detector: Arc::new(PaneProver { scanner, root }),
    }
}

async fn prompt_under(pane: &[PaneRow]) -> (WorkerInputResult, Vec<String>) {
    let session = Harness::new();
    session.install(SESSION, CHANNEL, "/home/user/project", "/home/user/project");
    let deps = foreground_deps(&session, pane).await;
    let proof = deps
        .registry
        .current_private_proof(&session_id(SESSION))
        .unwrap();
    let result = write_agent_prompt(&request_for(&proof), &TestBudget::live(), &deps).await;
    let written = session.keeper.input.written().into_iter();
    (
        result,
        written
            .map(|(_, bytes)| String::from_utf8(bytes).unwrap())
            .collect(),
    )
}

#[tokio::test]
async fn the_agent_holding_the_pane_foreground_job_is_admitted_tool_subprocess_included() {
    let panes: [&[PaneRow]; 2] = [
        &[(100, 1, 100, 200, "bash"), (200, 100, 200, 200, "omp")],
        &[
            (100, 1, 100, 300, "bash"),
            (200, 100, 200, 300, "omp"),
            (300, 200, 300, 300, "rg"),
        ],
    ];
    for pane in panes {
        let (result, written) = prompt_under(pane).await;
        assert_eq!(result, WorkerInputResult::Accepted { written_bytes: 9 });
        assert_eq!(written, ["continue", "\r"]);
    }
}

#[tokio::test]
async fn a_pane_foreground_job_outside_the_agent_subtree_rejects_without_writing() {
    let panes: [&[PaneRow]; 2] = [
        &[
            (100, 1, 100, 400, "bash"),
            (200, 100, 200, 400, "omp"),
            (400, 100, 400, 400, "vi"),
        ],
        &[(100, 1, 100, -1, "bash"), (200, 100, 200, -1, "omp")],
    ];
    for pane in panes {
        let (result, written) = prompt_under(pane).await;
        let reason = NOT_FOREGROUND_REASON.to_owned();
        assert_eq!(result, WorkerInputResult::Rejected { reason });
        assert!(
            written.is_empty(),
            "a rejected prompt reached the keeper: {written:?}"
        );
    }
}
