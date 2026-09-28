//! The shared fixture for the global-search tests: a real migrated database
//! with three worker rows, fake routable worker generations that capture every
//! browser command, and the real pending-RPC table the handlers settle through.
//!
//! Ports `apps/coord/tests/search/global-search-test-fixture.ts`. Each test
//! builds its own `Harness`, so no cursor, lane, or pending entry leaks.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::caller::{Caller, ListenerTrust};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::search::cursor_types::{
    GlobalSearchContinuation, GlobalSearchCursorBinding, GlobalSearchCursorIssue,
    GlobalSearchIdentity, GlobalSearchSessionPosition,
};
use roost_coord::search::options::normalize_global_search_page_limits;
use roost_coord::search::rpc_search::handle_sessions_search_global;
use roost_coord::services::CoordServices;
use roost_proto::{SessionsSearchGlobalRequest, SessionsSearchGlobalResponse};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::{Value, json};
use sqlx::AssertSqlSafe;
use tokio::task::JoinHandle;

pub type SearchAnswer = Result<SessionsSearchGlobalResponse, connectrpc::ConnectError>;

/// A first-page request with default limits.
pub fn search_request(
    query: &str,
    search_id: &str,
    cursor: Option<String>,
) -> SessionsSearchGlobalRequest {
    SessionsSearchGlobalRequest {
        query: query.to_owned(),
        search_id: search_id.to_owned(),
        cursor,
        ..Default::default()
    }
}

pub const WORKER_A1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const WORKER_A2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const WORKER_B: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// `globalSearchSessionId`.
pub fn session_id(sequence: u32) -> String {
    format!("00000000-0000-4000-8000-{sequence:012}")
}

/// `globalSearchResult`: one complete page with one match at row 5.
pub fn search_result(grid_epoch: &str, overrides: Value) -> Value {
    let mut result = json!({
        "matches": [{ "row": 5, "col": 1, "len": 6, "preview": "needle" }],
        "truncated": false,
        "scrollback_total": 10,
        "cols": 80,
        "grid_epoch": grid_epoch,
        "scanned_start_row": 0,
        "scanned_end_row": 10,
        "history_floor": "none",
        "stop_reason": "complete",
    });
    merge(&mut result, overrides);
    result
}

/// `globalSearchOkEntry`, epoch defaulting to `epoch-<last four>`.
pub fn ok_entry(id: &str, grid_epoch: Option<&str>, overrides: Value) -> Value {
    let default_epoch = format!("epoch-{}", &id[id.len() - 4..]);
    json!({
        "status": "ok",
        "session_id": id,
        "result": search_result(grid_epoch.unwrap_or(&default_epoch), overrides),
    })
}

pub fn merge(target: &mut Value, overrides: Value) {
    if let (Some(target), Value::Object(overrides)) = (target.as_object_mut(), overrides) {
        for (key, value) in overrides {
            target.insert(key, value);
        }
    }
}

/// One browser command a fake worker was sent.
#[derive(Debug, Clone)]
pub struct Captured {
    pub worker_fp: String,
    pub browser_id: String,
    pub viewer_id: String,
    pub request_id: String,
    pub control: Value,
}

/// A fake routable worker generation.
#[derive(Clone)]
pub struct TestWorker {
    pub worker_fp: String,
    commands: Arc<Mutex<Vec<Captured>>>,
    fail_sends: Arc<AtomicBool>,
}

impl TestWorker {
    pub fn commands(&self) -> Vec<Captured> {
        self.commands.lock().unwrap().clone()
    }

    pub fn of_kind(&self, kind: &str) -> Vec<Captured> {
        self.commands()
            .into_iter()
            .filter(|command| command.control["kind"] == kind)
            .collect()
    }

    /// `throwOnSend`: the transport drops every frame.
    pub fn fail_sends(&self, value: bool) {
        self.fail_sends.store(value, Ordering::SeqCst);
    }

    /// `waitForKind`: at least `count` commands of `kind`, within a second.
    pub async fn wait_for_kind(&self, kind: &str, count: usize) -> Vec<Captured> {
        for _ in 0..1_000 {
            let matching = self.of_kind(kind);
            if matching.len() >= count {
                return matching.into_iter().take(count).collect();
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!(
            "timed out waiting for {kind} x{count} on {}",
            self.worker_fp
        );
    }
}

pub struct Harness {
    pub core: CoordCore,
    root: PathBuf,
}

impl Harness {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-global-search-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let database = roost_coord::db::open(&root.join("coord.db")).await.unwrap();
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .unwrap();
        for (fp, label) in [
            (WORKER_A1, "worker-a1"),
            (WORKER_A2, "worker-a2"),
            (WORKER_B, "worker-b"),
        ] {
            sqlx::query(AssertSqlSafe(
                "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
                 VALUES (?1, ?2, 'linux', 0, 0, ?3)",
            ))
            .bind(fp)
            .bind(label)
            .bind(&tenant.dashboard_id)
            .execute(database.pool())
            .await
            .unwrap();
        }
        let services = Arc::new(CoordServices::new(database));
        Self {
            core: CoordCore::new(services),
            root,
        }
    }

    pub fn install_worker(&self, worker_fp: &str) -> TestWorker {
        let worker = TestWorker {
            worker_fp: worker_fp.to_owned(),
            commands: Arc::new(Mutex::new(Vec::new())),
            fail_sends: Arc::new(AtomicBool::new(false)),
        };
        let capture = worker.clone();
        let handle = WorkerHandle::new(
            WorkerFp::try_from(worker_fp).unwrap(),
            None,
            format!("gen-{worker_fp}"),
            Default::default(),
            Arc::new(move |frame: CoordWorkerDownstream| {
                if capture.fail_sends.load(Ordering::SeqCst) {
                    return 0;
                }
                if let CoordWorkerDownstream::BrowserCommand {
                    browser_id,
                    viewer_id,
                    request_id,
                    frame,
                    ..
                } = frame
                {
                    capture.commands.lock().unwrap().push(Captured {
                        worker_fp: capture.worker_fp.clone(),
                        browser_id,
                        viewer_id,
                        request_id,
                        control: serde_json::to_value(&frame).unwrap(),
                    });
                }
                1
            }),
        );
        assert!(handle.mark_ready());
        self.core.services.workers.insert(Arc::new(handle));
        worker
    }

    pub async fn insert_session(&self, id: &str, worker_fp: &str, status: &str, created_at: i64) {
        let dashboard_id: String =
            sqlx::query_scalar("SELECT dashboard_id FROM workers WHERE fp = ?1")
                .bind(worker_fp)
                .fetch_one(self.core.services.db.pool())
                .await
                .unwrap();
        let channel = i64::from_str_radix(&id[id.len() - 6..], 16).unwrap();
        sqlx::query(AssertSqlSafe(
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'shell', '/tmp', ?5, ?6)",
        ))
        .bind(id)
        .bind(dashboard_id)
        .bind(worker_fp)
        .bind(channel)
        .bind(status)
        .bind(created_at)
        .execute(self.core.services.db.pool())
        .await
        .unwrap();
    }

    pub async fn open_session(&self, id: &str, worker_fp: &str, created_at: i64) {
        self.insert_session(id, worker_fp, "open", created_at).await;
    }

    pub async fn execute(&self, sql: &'static str, bind: &str) {
        sqlx::query(sql)
            .bind(bind)
            .execute(self.core.services.db.pool())
            .await
            .unwrap();
    }

    /// The global-browser caller on tab `global-tab`, as v2's fixture context.
    pub fn caller(&self) -> Caller {
        self.caller_for("global-browser", Some("global-tab"))
    }

    pub fn caller_for(&self, device: &str, tab_id: Option<&str>) -> Caller {
        Caller {
            principal: Principal::AccountDevice {
                fingerprint: device.to_owned(),
                label: "Global browser".to_owned(),
                account_id: "account-1".to_owned(),
            },
            tab_id: tab_id.map(str::to_owned),
            remote_address: Some("127.0.0.1:51000".to_owned()),
            on_host: true,
            listener_trust: ListenerTrust::DirectLoopback,
        }
    }

    /// `respond`: settle a captured command's pending RPC with `data`.
    pub fn respond(&self, command: &Captured, data: Value) {
        assert!(
            self.core.services.scrollback.pending().resolve(
                &command.request_id,
                data,
                Some(&command.worker_fp),
            ),
            "global search pending RPC was not found"
        );
    }

    /// Run `SessionsSearchGlobal` on its own task, as a browser call would.
    pub fn spawn_search(
        &self,
        device: &str,
        tab_id: Option<&str>,
        req: SessionsSearchGlobalRequest,
    ) -> JoinHandle<SearchAnswer> {
        let core = self.core.clone();
        let caller = self.caller_for(device, tab_id);
        tokio::spawn(async move {
            handle_sessions_search_global(&core, &caller, req)
                .await
                .map(|response| response.body)
        })
    }

    /// `spawn_search` as the fixture's default caller.
    pub fn search(&self, req: SessionsSearchGlobalRequest) -> JoinHandle<SearchAnswer> {
        self.spawn_search("global-browser", Some("global-tab"), req)
    }

    /// Issue a cursor on the service's owner for the default caller and
    /// limits, over positions no page has searched.
    pub fn issue_cursor(
        &self,
        search_id: &str,
        query: &str,
        positions: Vec<GlobalSearchSessionPosition>,
        eligible_sessions: usize,
    ) -> String {
        self.core
            .services
            .search
            .cursors()
            .issue_cursor(GlobalSearchCursorIssue {
                binding: GlobalSearchCursorBinding {
                    identity: GlobalSearchIdentity {
                        device_fingerprint: "global-browser".to_owned(),
                        tab_id: "global-tab".to_owned(),
                        search_id: search_id.to_owned(),
                    },
                    query: query.to_owned(),
                    case_sensitive: false,
                    limits: normalize_global_search_page_limits(0, 0, 0),
                },
                continuations: positions
                    .into_iter()
                    .map(|position| GlobalSearchContinuation {
                        position,
                        searched: false,
                        requested_before_row: None,
                    })
                    .collect(),
                eligible_sessions,
                searched_session_ids: Vec::new(),
            })
            .unwrap()
    }

    pub fn pending(&self) -> usize {
        self.core.services.scrollback.pending().pending_count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
