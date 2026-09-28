//! A migrated coordinator database with one worker row, open sessions on it,
//! and a fake current worker generation whose input requests a test answers
//! through the real pending-request table. Shared by the `terminal_input_*`
//! binaries, which are separate crates and so cannot share a test module.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::services::CoordServices;
use roost_coord::terminal_input::control_lane::{TerminalViewerIdentity, terminal_viewer_identity};
use roost_coord::terminal_input::input_control::InputControlCommand;
use roost_coord::terminal_screen::typed_results::TypedWorkerResult;
use roost_proto::DInputRequest;
use roost_protocol::versioning::CAPABILITY_TERMINAL_INPUT_ROUTE_V1;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, InputResult, TerminalInputStatus, TerminalWritePhase,
};
use roost_protocol::wire::{SessionId, WorkerFp};
use sqlx::AssertSqlSafe;

pub const WORKER_FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
pub const CALLER_FP: &str = "cc00000000000000000000000000000000000000000000000000000000000000";

/// Every downstream frame the fake worker was sent.
pub type SentFrames = Arc<Mutex<Vec<CoordWorkerDownstream>>>;

/// How the fake worker answers one input request; `None` leaves it pending.
pub type InputReply = Arc<dyn Fn(&DInputRequest) -> Option<InputResult> + Send + Sync>;

/// A coordinator's services over a scratch database.
pub struct InputHarness {
    pub services: Arc<CoordServices>,
    pub sent: SentFrames,
    root: PathBuf,
    dashboard_id: String,
}

impl InputHarness {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-terminal-input-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .expect("the self-hosted tenant");
        sqlx::query(AssertSqlSafe(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES (?1, 'laptop', 'linux', 0, 0, ?2)",
        ))
        .bind(WORKER_FP)
        .bind(&tenant.dashboard_id)
        .execute(database.pool())
        .await
        .expect("a worker row");
        Self {
            services: Arc::new(CoordServices::new(database)),
            sent: SentFrames::default(),
            root,
            dashboard_id: tenant.dashboard_id,
        }
    }

    /// One open session on channel `channel` of the worker.
    pub async fn seed_session(&self, tail: u32, channel: i64) -> String {
        let id = session_id(tail);
        sqlx::query(AssertSqlSafe(
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'shell', '/tmp', 'open', 0)",
        ))
        .bind(&id)
        .bind(&self.dashboard_id)
        .bind(WORKER_FP)
        .bind(channel)
        .execute(self.services.db.pool())
        .await
        .expect("an open session row");
        id
    }

    /// Make a ready, route-capable generation current, answering each input
    /// request with `reply` through the pending-request table.
    pub fn attach_worker(&self, reply: InputReply) -> Arc<WorkerHandle> {
        let sent = Arc::clone(&self.sent);
        let pending = Arc::clone(self.services.scrollback.pending());
        let handle = WorkerHandle::new(
            WorkerFp::try_from(WORKER_FP).unwrap(),
            Some("worker-epoch-a".to_owned()),
            "worker-connection-a".to_owned(),
            BTreeSet::from([CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned()]),
            Arc::new(move |frame: CoordWorkerDownstream| {
                if let CoordWorkerDownstream::InputRequest(request) = &frame
                    && let Some(result) = reply(request)
                {
                    pending.resolve_typed(TypedWorkerResult::Input(result), Some(WORKER_FP));
                }
                sent.lock().unwrap().push(frame);
                1
            }),
        );
        handle.mark_ready();
        let handle = Arc::new(handle);
        self.services.workers.insert(Arc::clone(&handle));
        handle
    }

    pub fn input_requests(&self) -> Vec<DInputRequest> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|frame| match frame {
                CoordWorkerDownstream::InputRequest(request) => Some(request.clone()),
                _ => None,
            })
            .collect()
    }
}

impl Drop for InputHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn session_id(tail: u32) -> String {
    format!("00000000-0000-4000-8000-{tail:012}")
}

pub fn identity(tab_id: &str) -> TerminalViewerIdentity {
    terminal_viewer_identity(CALLER_FP, Some(tab_id))
}

/// A unary-shaped batch: no route authority, no audit, a fresh budget.
pub fn batch(tab_id: &str, session_id: &str, input_seq: u64, data: &[u8]) -> InputControlCommand {
    InputControlCommand {
        identity: identity(tab_id),
        session_id: session_id.to_owned(),
        input_seq,
        data: data.to_vec(),
        socket_generation: None,
        input_route_authority: None,
        audited: false,
        deadline: None,
    }
}

/// The keeper wrote every byte of the request.
pub fn written_in_full(request: &DInputRequest) -> Option<InputResult> {
    Some(InputResult {
        request_id: request.request_id.clone(),
        session_id: SessionId::try_from(request.session_id.as_str()).unwrap(),
        input_seq: request.input_seq,
        status: TerminalInputStatus::Accepted,
        written_bytes: u32::try_from(request.data.len()).unwrap(),
        reason: String::new(),
        phase: TerminalWritePhase::Written,
    })
}
