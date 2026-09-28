//! The collaborators agent-prompt admission is exercised through: a real
//! session manager over the scripted keeper (`session_support`), a status proof
//! the test replaces the way the registry would, a process prover that answers
//! from a hook, and a request budget the test moves. Shared by the
//! `agent_prompt_*` suites, which port `apps/worker/tests/agents/agent-prompt-*.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

pub mod log_capture;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use roost_proto::DAgentPrompt;
use roost_protocol::wire::agent_status::{
    AgentOccupantId, AgentRuntimeState, AgentStatusSource, StatusEpoch,
};
use roost_protocol::wire::brand::SessionId;
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::process_scan::{AgentProcessIdentity, ScanAbort};
use roost_worker::agents::process_tree::AgentForegroundJob;
use roost_worker::agents::prompt_control::{
    AgentProcessProver, AgentPromptControlDeps, AgentStatusProofs, write_agent_prompt,
};
use roost_worker::agents::prompt_fence::PromptBudget;
use roost_worker::agents::registry::AgentStatusPrivateProof;
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::session::keeper_admission::{Admission, AdmissionKind, AdmissionTicket};
use roost_worker::session::table::SessionTable;
use roost_worker::uplink::OwnerFuture;
use tokio::task::JoinHandle;

use crate::session_support::{Harness, SESSION, channel, session_id};

pub const CHANNEL: u16 = 1;
pub const AGENT_PID: u32 = 4_242;
const EPOCH: &str = "00000000-0000-4000-8000-0000000e90c0";
const OCCUPANT: &str = "00000000-0000-4000-8000-0000000cc0a1";
const OTHER_OCCUPANT: &str = "00000000-0000-4000-8000-0000000cc0a2";

/// The agent as a live snapshot row proved it: the agent holds the foreground.
pub fn process_proof() -> AgentProcessIdentity {
    AgentProcessIdentity {
        agent_id: BuiltinAgentId::Omp,
        pid: AGENT_PID,
        foreground: Some(AgentForegroundJob { group_id: AGENT_PID as i32, agent_member_pid: AGENT_PID }),
    }
}

/// An integration-reported idle `omp`, as the registry's private proof carries it.
pub fn integration_proof() -> AgentStatusPrivateProof {
    AgentStatusPrivateProof {
        status_epoch: StatusEpoch::try_from(EPOCH).unwrap(),
        occupant_id: AgentOccupantId::try_from(OCCUPANT).unwrap(),
        revision: 1_700_000_000_000_000,
        state: AgentRuntimeState::Idle,
        source: AgentStatusSource::Integration,
        process: AgentProcessIdentity { agent_id: BuiltinAgentId::Omp, pid: AGENT_PID, foreground: None },
    }
}

/// The registry's answer, replaced the way a report would replace it.
#[derive(Default)]
pub struct ScriptedStatus(Mutex<Option<AgentStatusPrivateProof>>);

impl ScriptedStatus {
    pub fn set(&self, proof: Option<AgentStatusPrivateProof>) {
        *self.0.lock().unwrap() = proof;
    }
    /// A newer report from the same occupant: the revision moves on.
    pub fn bump_revision(&self) {
        let mut held = self.0.lock().unwrap();
        let proof = held.as_mut().expect("a proof to bump");
        proof.revision += 1;
        proof.state = AgentRuntimeState::Working;
    }
    /// A different process took the session over: a new occupant.
    pub fn replace_occupant(&self) {
        let mut held = self.0.lock().unwrap();
        let proof = held.as_mut().expect("a proof to replace");
        proof.occupant_id = AgentOccupantId::try_from(OTHER_OCCUPANT).unwrap();
        proof.process.pid = AGENT_PID + 1;
    }
}

impl AgentStatusProofs for ScriptedStatus {
    fn current_private_proof(&self, session_id: &SessionId) -> Option<AgentStatusPrivateProof> {
        (session_id.as_str() == SESSION).then(|| self.0.lock().unwrap().clone()).flatten()
    }
}

/// One process refresh, as the prover's hook sees it.
pub struct RefreshCall {
    pub call: u32,
    pub reporter_pid: u32,
    pub abort: Option<ScanAbort>,
}

pub type RefreshHook = Box<dyn Fn(RefreshCall) -> OwnerFuture<Option<AgentProcessIdentity>> + Send + Sync>;

/// The detector's `reporting_agent_for_session`, answered by a hook.
pub struct ScriptedProver {
    calls: AtomicU32,
    hook: RefreshHook,
}

impl ScriptedProver {
    pub fn answering(hook: RefreshHook) -> Arc<Self> {
        Arc::new(Self { calls: AtomicU32::new(0), hook })
    }
    /// Proves the agent for its own pid, and nothing for any other.
    pub fn proving() -> Arc<Self> {
        Self::answering(Box::new(|call| {
            let proof = (call.reporter_pid == AGENT_PID).then(process_proof);
            Box::pin(std::future::ready(proof))
        }))
    }
    pub fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentProcessProver for ScriptedProver {
    fn reporting_agent_for_session(
        &self,
        _session_id: &SessionId,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        (self.hook)(RefreshCall { call, reporter_pid, abort })
    }
}

/// v2's `liveBudget` and its deadline form: current or superseded, and a
/// fixed remainder or one that runs down against a deadline.
#[derive(Clone)]
pub struct TestBudget {
    pub current: Arc<AtomicBool>,
    deadline: Arc<Mutex<Result<Instant, Duration>>>,
}

impl TestBudget {
    pub fn live() -> Self {
        Self::fixed(Duration::from_millis(5_000))
    }
    pub fn fixed(remaining: Duration) -> Self {
        Self { current: Arc::new(AtomicBool::new(true)), deadline: Arc::new(Mutex::new(Err(remaining))) }
    }
    pub fn until(deadline: Instant) -> Self {
        Self { current: Arc::new(AtomicBool::new(true)), deadline: Arc::new(Mutex::new(Ok(deadline))) }
    }
    pub fn spend(&self) {
        *self.deadline.lock().unwrap() = Err(Duration::ZERO);
    }
    pub fn supersede(&self) {
        self.current.store(false, Ordering::SeqCst);
    }
}

impl PromptBudget for TestBudget {
    fn is_current_connection(&self) -> bool {
        self.current.load(Ordering::SeqCst)
    }
    fn remaining(&self) -> Duration {
        match *self.deadline.lock().unwrap() {
            Ok(deadline) => deadline.saturating_duration_since(Instant::now()),
            Err(fixed) => fixed,
        }
    }
}

/// A live session holding an integration-reported agent.
pub struct PromptHarness {
    pub session: Harness,
    pub status: Arc<ScriptedStatus>,
    pub prover: Arc<ScriptedProver>,
    pub deps: Arc<AgentPromptControlDeps>,
}

impl PromptHarness {
    pub fn new(prover: Arc<ScriptedProver>) -> Self {
        Self::with_prover(|_| prover)
    }

    /// For a prover whose hook reaches the session table (a hook that changes
    /// the pane between two scans).
    pub fn with_prover(make: impl FnOnce(Arc<SessionTable>) -> Arc<ScriptedProver>) -> Self {
        let session = Harness::new();
        session.install(SESSION, CHANNEL, "/home/user/project", "/home/user/project");
        let prover = make(Arc::clone(&session.table));
        let status = Arc::new(ScriptedStatus::default());
        status.set(Some(integration_proof()));
        let deps = Arc::new(AgentPromptControlDeps {
            manager: Arc::clone(&session.manager),
            sessions: Arc::clone(&session.table),
            registry: Arc::clone(&status) as Arc<dyn AgentStatusProofs>,
            detector: Arc::clone(&prover) as Arc<dyn AgentProcessProver>,
        });
        Self { session, status, prover, deps }
    }

    /// The proof the registry holds now, which a request quotes.
    pub fn status_proof(&self) -> AgentStatusPrivateProof {
        self.status.0.lock().unwrap().clone().expect("the harness reports an agent")
    }

    /// Every acknowledged batch that reached the keeper, as text.
    pub fn written(&self) -> Vec<String> {
        self.session
            .keeper
            .input
            .written()
            .into_iter()
            .map(|(_, bytes)| String::from_utf8(bytes).unwrap())
            .collect()
    }

    /// The prompt, on its own task, so the test can act while it is queued.
    pub fn spawn_prompt(&self, request: DAgentPrompt, budget: TestBudget) -> JoinHandle<WorkerInputResult> {
        let deps = Arc::clone(&self.deps);
        tokio::spawn(async move { write_agent_prompt(&request, &budget, &deps).await })
    }

    /// Coordinator input behind whatever holds the lane now.
    pub fn spawn_raw(&self, bytes: &[u8]) -> JoinHandle<WorkerInputResult> {
        let written = self.session.manager.write_terminal_input(&session_id(SESSION), 2, bytes.to_vec(), None, None);
        tokio::spawn(written)
    }

    /// Hold the keeper write lane the way a stream transaction's resize does.
    pub async fn hold_lane(&self) -> AdmissionTicket {
        let Admission::Granted(ticket) = self
            .session
            .manager
            .control_lanes()
            .admit(channel(i64::from(CHANNEL)), AdmissionKind::TerminalResize)
        else {
            panic!("the lane admits a resize");
        };
        ticket.granted().await;
        ticket
    }

    /// Writers queued on or holding the lane, and which kind holds it.
    pub fn lane(&self) -> (u32, Option<AdmissionKind>) {
        let snapshot = self.session.manager.control_lanes().snapshot(channel(i64::from(CHANNEL)));
        (snapshot.admission_depth, snapshot.admission_holder)
    }
}

/// Poll until `ready` holds; a condition that never does fails the test.
pub async fn eventually(what: &str, ready: impl Fn() -> bool) {
    for _ in 0..2_000 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("{what} never happened");
}

/// A request quoting the proof the registry holds now.
pub fn request_for(proof: &AgentStatusPrivateProof) -> DAgentPrompt {
    DAgentPrompt {
        request_id: "agent-prompt-request".to_owned(),
        session_id: SESSION.to_owned(),
        input_seq: 1,
        expected_status_epoch: proof.status_epoch.as_str().to_owned(),
        expected_occupant_id: proof.occupant_id.as_str().to_owned(),
        expected_revision: u64::try_from(proof.revision).unwrap(),
        text: "continue".to_owned(),
        budget_ms: 5_000,
        ..DAgentPrompt::default()
    }
}
