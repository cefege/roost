//! The agent-prompt tests' shared fixture: a migrated install with two workers,
//! the prompted session and a second worker's session, the exact request
//! builder, retained agent status, and a fake current worker generation whose
//! agent-prompt frames a test answers through the real pending-request table.
//! Shared by `agent_prompt_handlers.rs` and `agent_prompt_status_wait.rs`.
//! Ports `apps/coord/tests/agents/agent-prompt-test-fixture.ts`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::agents::status_hub::AgentStatusAcceptance;
use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::services::CoordServices;
use roost_coord::terminal_input::control_lane::resolve_session_route;
use roost_coord::terminal_screen::typed_results::TypedWorkerResult;
use roost_proto::{DAgentPrompt, SessionsPromptRequest};
use roost_protocol::versioning::CAPABILITY_TERMINAL_INPUT_ROUTE_V1;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, InputResult, TerminalInputStatus, TerminalWritePhase,
};
use roost_protocol::wire::{SessionId, WorkerFp};
use serde_json::json;
use sqlx::AssertSqlSafe;

pub const PROMPT_WORKER: &str = "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";
pub const FOREIGN_WORKER: &str = "b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6";
pub const PROMPT_SESSION: &str = "81000000-0000-4000-8000-000000000001";
pub const FOREIGN_SESSION: &str = "81000000-0000-4000-8000-000000000002";
pub const MISSING_SESSION: &str = "81000000-0000-4000-8000-000000000099";
pub const PROMPT_STATUS_EPOCH: &str = "82000000-0000-4000-8000-000000000001";
pub const PROMPT_OCCUPANT: &str = "83000000-0000-4000-8000-000000000001";
const CALLER_FP: &str = "agent-prompt-device";

/// How the fake worker answers one prompt; `None` leaves it pending. The core
/// lets a reply publish agent status mid-send, as a real worker can.
pub type PromptReply = Arc<dyn Fn(&CoordCore, &DAgentPrompt) -> Option<InputResult> + Send + Sync>;

pub struct PromptHarness {
    pub services: Arc<CoordServices>,
    pub core: CoordCore,
    pub sent: Arc<Mutex<Vec<DAgentPrompt>>>,
    root: PathBuf,
}

impl PromptHarness {
    pub async fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("roost-agent-prompt-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = super::db_support::open_test_database(&root)
            .await
            .expect("a migrated database");
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .expect("the self-hosted tenant");
        for (worker, session, channel) in [
            (PROMPT_WORKER, PROMPT_SESSION, 41),
            (FOREIGN_WORKER, FOREIGN_SESSION, 42),
        ] {
            sqlx::query(AssertSqlSafe(
                "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
                 VALUES ($1, 'w', 'linux', 0, 0, $2)",
            ))
            .bind(worker)
            .bind(&tenant.dashboard_id)
            .execute(database.pool())
            .await
            .expect("a worker row");
            sqlx::query(AssertSqlSafe(
                "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
                 VALUES ($1, $2, $3, $4, 'shell', '/tmp', 'open', 0)",
            ))
            .bind(session)
            .bind(&tenant.dashboard_id)
            .bind(worker)
            .bind(channel)
            .execute(database.pool())
            .await
            .expect("an open session row");
        }
        let services = Arc::new(CoordServices::new(database));
        // Bind the prompted session's route so the hub admits its status.
        resolve_session_route(&services.db, &services.byte_hub, PROMPT_SESSION)
            .await
            .unwrap()
            .expect("the prompted session's route");
        let harness = Self {
            core: CoordCore::new(Arc::clone(&services)),
            services,
            sent: Arc::default(),
            root,
        };
        harness.retain_status("working", 1, None, 0);
        harness
    }

    pub fn caller(&self) -> Caller {
        Caller {
            principal: Principal::AccountDevice {
                fingerprint: CALLER_FP.to_owned(),
                label: "prompt test device".to_owned(),
                account_id: "agent-prompt-account".to_owned(),
            },
            tab_id: Some("prompt-test-tab".to_owned()),
            remote_address: None,
            on_host: true,
            listener_trust: ListenerTrust::DirectLoopback,
        }
    }

    pub fn retain_status(&self, state: &str, revision: i64, message: Option<&str>, completed: i64) {
        retain_status(&self.core, state, revision, message, completed);
    }

    /// Make the prompt worker's ready generation current.
    pub fn attach_worker(&self, reply: PromptReply) {
        let sent = Arc::clone(&self.sent);
        let services = Arc::clone(&self.services);
        let handle = WorkerHandle::new(
            WorkerFp::try_from(PROMPT_WORKER).unwrap(),
            Some("worker-epoch-a".to_owned()),
            "worker-connection-a".to_owned(),
            BTreeSet::from([CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned()]),
            Arc::new(move |frame: CoordWorkerDownstream| {
                if let CoordWorkerDownstream::AgentPrompt(prompt) = &frame {
                    sent.lock().unwrap().push(prompt.clone());
                    let core = CoordCore::new(Arc::clone(&services));
                    if let Some(result) = reply(&core, prompt) {
                        services
                            .scrollback
                            .pending()
                            .resolve_typed(TypedWorkerResult::Input(result), Some(PROMPT_WORKER));
                    }
                }
                1
            }),
        );
        handle.mark_ready();
        self.services.workers.insert(Arc::new(handle));
    }

    pub fn prompts(&self) -> Vec<DAgentPrompt> {
        self.sent.lock().unwrap().clone()
    }

    pub fn wait_count(&self) -> usize {
        self.services.agents.status.wait_count()
    }
}

impl Drop for PromptHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Publish one integration status frame for the pinned occupant.
pub fn retain_status(
    core: &CoordCore,
    state: &str,
    revision: i64,
    message: Option<&str>,
    completed: i64,
) {
    let mut frame = json!({
        "session_id": PROMPT_SESSION,
        "agent_id": "omp",
        "state": state,
        "revision": revision,
        "completed_revision": completed,
        "updated_at": 1_800_000_000_000_i64 + revision,
        "active": true,
        "status_epoch": PROMPT_STATUS_EPOCH,
        "occupant_id": PROMPT_OCCUPANT,
        "source": "integration",
    });
    if let Some(message) = message {
        frame["message"] = json!(message);
    }
    let worker = WorkerFp::try_from(PROMPT_WORKER).unwrap();
    let accepted = core
        .services
        .agents
        .status
        .accept_worker_status(core, &worker, frame);
    assert_eq!(accepted, AgentStatusAcceptance::Accepted, "status fixture");
}

/// The default well-formed prompt.
pub fn request() -> SessionsPromptRequest {
    SessionsPromptRequest {
        session_id: PROMPT_SESSION.to_owned(),
        expected_status_epoch: PROMPT_STATUS_EPOCH.to_owned(),
        expected_occupant_id: PROMPT_OCCUPANT.to_owned(),
        expected_revision: 1,
        text: "continue".to_owned(),
        ..Default::default()
    }
}

/// The worker's keeper-proven result for `prompt`.
pub fn result(
    prompt: &DAgentPrompt,
    status: TerminalInputStatus,
    phase: TerminalWritePhase,
    written_bytes: u32,
    reason: &str,
) -> InputResult {
    InputResult {
        request_id: prompt.request_id.clone(),
        session_id: SessionId::try_from(prompt.session_id.as_str()).unwrap(),
        input_seq: prompt.input_seq,
        status,
        written_bytes,
        reason: reason.to_owned(),
        phase,
    }
}

/// A reply that answers every prompt with the same outcome.
pub fn answer(
    status: TerminalInputStatus,
    phase: TerminalWritePhase,
    bytes: u32,
    reason: &str,
) -> PromptReply {
    let reason = reason.to_owned();
    Arc::new(move |_core, prompt| Some(result(prompt, status, phase, bytes, &reason)))
}
