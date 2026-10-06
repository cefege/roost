//! The DiagSnapshot tests' fixture: a migrated coordinator with a self-hosted
//! tenant, three live workers (A and C own 64 batch sessions between them,
//! LOCAL owns one more), and fake worker sockets that answer the
//! `diag-snapshot` browser command and the typed pipeline sample.
//!
//! Ports `apps/coord/tests/diagnostics/diag-snapshot-harness.ts`. Shared by
//! `diag_snapshot_filters.rs`, `diag_snapshot_fanout.rs` and
//! `diag_snapshot_session_state.rs`, which are separate crates.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::auth::principal::Principal;
use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::coord_core::{BootFacts, Caller, CoordCore, ListenerTrust};
use roost_coord::services::CoordServices;
use roost_coord::terminal_screen::pipeline_cache::WorkerTerminalPipelineSnapshotCache;
use roost_coord::terminal_screen::typed_results::TypedWorkerResult;
use roost_proto::{TerminalPipelineSessionSnapshot, WTerminalPipelineSnapshot};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::{Value, json};

pub const WORKER_A: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
pub const WORKER_C: &str = "cc00000000000000000000000000000000000000000000000000000000000000";
pub const WORKER_LOCAL: &str = "dd00000000000000000000000000000000000000000000000000000000000000";
pub const DIAG_WORKER_FPS: [&str; 3] = [WORKER_A, WORKER_C, WORKER_LOCAL];
pub const LOCAL_UNSELECTED_SESSION: &str = "0e000000-0000-4000-8000-000000000001";
pub const MISSING_SESSION: &str = "0f000000-0000-4000-8000-000000000001";

/// The 64 batch sessions: even indexes on WORKER_A, odd on WORKER_C.
pub fn batch_session_ids() -> Vec<String> {
    (0..64)
        .map(|index| format!("0a000000-0000-4000-8000-{index:012}"))
        .collect()
}

/// How a fake worker's socket treats a `diag-snapshot` command.
#[derive(Clone)]
pub enum DiagReply {
    /// Answer with this body under the worker's own fingerprint.
    Answer(Value),
    /// Take the frame and never answer.
    Silent,
}

/// Every command the fake workers were sent.
#[derive(Default)]
pub struct DiagWorkerLog {
    pub sent_worker_fps: Vec<String>,
    pub pipeline_targets: BTreeMap<String, Vec<String>>,
    pub diag_request_ids: BTreeMap<String, String>,
}

pub struct DiagFixture {
    pub core: CoordCore,
    pub pipelines: WorkerTerminalPipelineSnapshotCache,
    pub log: Arc<Mutex<DiagWorkerLog>>,
    pub handles: Vec<Arc<WorkerHandle>>,
    root: PathBuf,
}

impl DiagFixture {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-diag-snapshot-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = super::db_support::open_test_database(&root)
            .await
            .expect("a migrated coordinator database");
        let tenant = ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("a self-hosted tenant");
        let dashboard_id = tenant.dashboard_id.clone();
        let services = CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                ..BootFacts::unbooted()
            },
        );
        let core = CoordCore::new(Arc::new(services));
        let pipelines = WorkerTerminalPipelineSnapshotCache::new(core.services.scrollback.clone());
        let fixture = Self {
            core,
            pipelines,
            log: Arc::new(Mutex::new(DiagWorkerLog::default())),
            handles: Vec::new(),
            root,
        };
        for worker_fp in DIAG_WORKER_FPS {
            fixture.exec(&format!(
                "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
                 VALUES ('{worker_fp}', 'w', 'linux', 1000, 1000, '{dashboard_id}')"
            ))
            .await;
        }
        for (index, session_id) in batch_session_ids().iter().enumerate() {
            let worker_fp = if index % 2 == 0 { WORKER_A } else { WORKER_C };
            fixture
                .seed_session(&dashboard_id, session_id, worker_fp, index as i64)
                .await;
        }
        fixture
            .seed_session(&dashboard_id, LOCAL_UNSELECTED_SESSION, WORKER_LOCAL, 99)
            .await;
        fixture
    }

    async fn seed_session(&self, dashboard_id: &str, id: &str, worker_fp: &str, channel: i64) {
        self.exec(&format!(
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
             VALUES ('{id}', '{dashboard_id}', '{worker_fp}', {channel}, 'shell', '/tmp', 'open', 0)"
        ))
        .await;
    }

    pub async fn exec(&self, sql: &str) {
        sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(self.core.services.db.pool())
            .await
            .expect("fixture SQL runs");
    }

    /// Registers a ready generation for `worker_fp` whose socket records every
    /// command, answers pipeline samples in full, and treats `diag-snapshot`
    /// as `reply` says.
    pub fn connect(&mut self, worker_fp: &str, reply: DiagReply) -> Arc<WorkerHandle> {
        let relay = self.core.services.scrollback.clone();
        let log = Arc::clone(&self.log);
        let fp = worker_fp.to_owned();
        let handle = Arc::new(WorkerHandle::new(
            WorkerFp::try_from(worker_fp).unwrap(),
            None,
            format!("{worker_fp}-generation"),
            Default::default(),
            Arc::new(move |frame: CoordWorkerDownstream| {
                match frame {
                    CoordWorkerDownstream::BrowserCommand {
                        request_id,
                        frame: ClientControlFrame::DiagSnapshot { .. },
                        ..
                    } => {
                        {
                            let mut log = log.lock().unwrap();
                            log.sent_worker_fps.push(fp.clone());
                            log.diag_request_ids.insert(fp.clone(), request_id.clone());
                        }
                        if let DiagReply::Answer(body) = &reply {
                            relay
                                .pending()
                                .resolve(&request_id, body.clone(), Some(&fp));
                        }
                    }
                    CoordWorkerDownstream::TerminalPipelineSnapshot(request) => {
                        let sessions: Vec<String> = request
                            .targets
                            .iter()
                            .map(|target| target.session_id.clone())
                            .collect();
                        log.lock()
                            .unwrap()
                            .pipeline_targets
                            .insert(fp.clone(), sessions);
                        let reply = WTerminalPipelineSnapshot {
                            request_id: request.request_id.clone(),
                            sessions: request
                                .targets
                                .iter()
                                .map(|target| TerminalPipelineSessionSnapshot {
                                    session_id: target.session_id.clone(),
                                    view_id: target.view_id.clone(),
                                    ..Default::default()
                                })
                                .collect(),
                            ..Default::default()
                        };
                        relay
                            .pending()
                            .resolve_typed(TypedWorkerResult::PipelineSnapshot(reply), Some(&fp));
                    }
                    other => panic!("unexpected frame {}", other.kind()),
                }
                1
            }),
        ));
        assert!(handle.mark_ready());
        self.core.services.workers.insert(Arc::clone(&handle));
        self.handles.push(Arc::clone(&handle));
        handle
    }

    /// Every fake worker answers with a snapshot naming every batch session.
    pub fn connect_all_answering(&mut self) {
        for worker_fp in DIAG_WORKER_FPS {
            self.connect(worker_fp, DiagReply::Answer(worker_snapshot(worker_fp)));
        }
    }

    pub fn log(&self) -> std::sync::MutexGuard<'_, DiagWorkerLog> {
        self.log.lock().unwrap()
    }
}

impl Drop for DiagFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A worker snapshot naming its own batch sessions plus two it must never
/// leak (an unselected session and an unknown one), so scoping has work to do.
pub fn worker_snapshot(worker_fp: &str) -> Value {
    let owned_parity = match worker_fp {
        WORKER_A => Some(0),
        WORKER_C => Some(1),
        _ => None,
    };
    let mut sessions = serde_json::Map::new();
    for (index, session_id) in batch_session_ids().into_iter().enumerate() {
        if owned_parity == Some(index % 2) {
            sessions.insert(session_id, json!({ "worker": worker_fp }));
        }
    }
    for session_id in [LOCAL_UNSELECTED_SESSION, MISSING_SESSION] {
        sessions.insert(session_id.to_owned(), json!({ "worker": worker_fp }));
    }
    json!({
        "captured_at_ms": 5,
        "build": { "git_sha": "worker-build" },
        "worker_fp": "spoofed",
        "sessions": sessions,
        "secrets": "never forwarded",
    })
}

/// The session ids every ok worker envelope returned, sorted.
pub fn returned_session_ids(snapshot: &Value) -> Vec<String> {
    let mut ids: Vec<String> = snapshot["workers"]
        .as_object()
        .unwrap()
        .values()
        .filter(|envelope| envelope["status"] == "ok")
        .flat_map(|envelope| {
            envelope["snapshot"]["sessions"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();
    ids.sort();
    ids
}

/// The session ids every pipeline envelope returned, sorted.
pub fn returned_pipeline_session_ids(snapshot: &Value) -> Vec<String> {
    let mut ids: Vec<String> = snapshot["workers"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|envelope| envelope["terminal_pipeline"]["snapshot"]["sessions"].as_array())
        .flatten()
        .map(|session| session["session_id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

pub fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

/// A paired browser: the authority DiagSnapshot admits.
pub fn device() -> Caller {
    caller(Principal::AccountDevice {
        fingerprint: "fp-diag".to_owned(),
        label: "diag pane".to_owned(),
        account_id: "account-under-test".to_owned(),
    })
}

/// A registered machine: not the operator.
pub fn worker_caller() -> Caller {
    caller(Principal::Worker {
        fingerprint: "diagnostics-worker".to_owned(),
        label: "a worker".to_owned(),
    })
}

fn caller(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: Some("tab-under-test".to_owned()),
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}
