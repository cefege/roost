//! The shared harness for the session lifecycle RPC tests: a booted coordinator
//! over a real migrated database, one registered worker whose live socket
//! generation records every downstream frame, and the rows a test seeds.
//!
//! Shared by the `sessions_*` binaries, because `tests/*.rs` are independent
//! crates and a harness has to live in a module each can include.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::caller::{Caller, ListenerTrust};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::services::CoordServices;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use sqlx::AssertSqlSafe;

pub const WORKER_FP: &str = "5e55000000000000000000000000000000000000000000000000000000000000";
pub const BROWSER_FP: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
pub const TAB: &str = "tab-a";

/// One downstream browser command, as the worker would read it.
#[derive(Debug, Clone)]
pub struct SentCommand {
    pub browser_id: String,
    pub viewer_id: String,
    pub request_id: String,
    pub frame: ClientControlFrame,
}

/// A coordinator with one registered worker and, once `connect_worker` runs, a
/// live socket generation for it.
pub struct SessionsHarness {
    pub core: CoordCore,
    pub dashboard_id: String,
    root: PathBuf,
    sent: Arc<Mutex<Vec<CoordWorkerDownstream>>>,
    handle: Mutex<Option<Arc<WorkerHandle>>>,
}

impl SessionsHarness {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-sessions-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("the self-hosted tenant");
        let dashboard_id = tenant.dashboard_id.clone();
        let services = Arc::new(CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                config: None,
                process_epoch: "epoch-1".to_owned(),
                boot_ms: 0,
            },
        ));
        let harness = Self {
            core: CoordCore::new(services),
            dashboard_id,
            root,
            sent: Arc::new(Mutex::new(Vec::new())),
            handle: Mutex::new(None),
        };
        harness
            .exec(&format!(
                "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
                 VALUES ('{WORKER_FP}', 'laptop', 'linux', 0, 0, '{}')",
                harness.dashboard_id
            ))
            .await;
        harness
    }

    /// Give the worker a live, ready socket generation.
    pub fn connect_worker(&self) -> Arc<WorkerHandle> {
        let sent = Arc::clone(&self.sent);
        let handle = Arc::new(WorkerHandle::new(
            WorkerFp::try_from(WORKER_FP).unwrap(),
            None,
            format!("gen-{}", self.sent.lock().unwrap().len()),
            Default::default(),
            Arc::new(move |frame: CoordWorkerDownstream| {
                let mut sent = sent.lock().unwrap();
                sent.push(frame);
                i64::try_from(sent.len()).unwrap()
            }),
        ));
        assert!(handle.mark_ready(), "the generation crossed its barrier");
        self.core.services.workers.insert(Arc::clone(&handle));
        *self.handle.lock().unwrap() = Some(Arc::clone(&handle));
        handle
    }

    pub fn device(&self, tab: Option<&str>) -> Caller {
        Caller {
            principal: Principal::AccountDevice {
                fingerprint: BROWSER_FP.to_owned(),
                label: "laptop".to_owned(),
                account_id: "account-1".to_owned(),
            },
            tab_id: tab.map(str::to_owned),
            remote_address: Some("127.0.0.1:51000".to_owned()),
            on_host: true,
            listener_trust: ListenerTrust::DirectLoopback,
        }
    }

    pub async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql.to_owned()))
            .execute(self.core.services.db.pool())
            .await
            .expect("the statement applies");
    }

    pub async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(AssertSqlSafe(sql.to_owned()))
            .fetch_one(self.core.services.db.pool())
            .await
            .expect("the scalar reads")
    }

    pub async fn text(&self, sql: &str) -> Option<String> {
        sqlx::query_scalar(AssertSqlSafe(sql.to_owned()))
            .fetch_one(self.core.services.db.pool())
            .await
            .expect("the value reads")
    }

    pub async fn seed_session(&self, id: &str, channel: i64) {
        self.exec(&format!(
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
             VALUES ('{id}', '{}', '{WORKER_FP}', {channel}, 'shell', '/tmp', 'open', 0)",
            self.dashboard_id
        ))
        .await;
    }

    pub async fn seed_workspace(&self, id: &str) {
        self.exec(&format!(
            "INSERT INTO workspaces (id, dashboard_id, worker_fp, name, folder_path, color, position, \
             version, created_at_ms, updated_at_ms) \
             VALUES ('{id}', '{}', '{WORKER_FP}', 'ws', '/tmp/{id}', 'blue', 0, 1, 0, 0)",
            self.dashboard_id
        ))
        .await;
    }

    /// Every browser command the worker was sent, in order.
    pub fn commands(&self) -> Vec<SentCommand> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|frame| match frame {
                CoordWorkerDownstream::BrowserCommand {
                    browser_id,
                    viewer_id,
                    request_id,
                    frame,
                    ..
                } => Some(SentCommand {
                    browser_id: browser_id.clone(),
                    viewer_id: viewer_id.clone(),
                    request_id: request_id.clone(),
                    frame: frame.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Wait until command `count` was sent and return it.
    pub async fn command(&self, count: usize) -> SentCommand {
        for _ in 0..2_000 {
            if let Some(command) = self.commands().get(count - 1) {
                return command.clone();
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!(
            "command {count} was never sent; saw {}",
            self.commands().len()
        );
    }

    /// Answer a command as the worker's `rpc-ok`.
    pub fn reply_ok(&self, request_id: &str, data: serde_json::Value) {
        assert!(
            self.core
                .services
                .scrollback
                .pending()
                .resolve(request_id, data, Some(WORKER_FP)),
            "a pending RPC was waiting for {request_id}"
        );
    }

    /// Answer a command as the worker's `rpc-error`.
    pub fn reply_error(&self, request_id: &str, message: &str) {
        assert!(
            self.core
                .services
                .scrollback
                .pending()
                .reject(request_id, message, Some(WORKER_FP)),
            "a pending RPC was waiting for {request_id}"
        );
    }
}

impl Drop for SessionsHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A session UUID with a readable tail.
pub fn session(tail: &str) -> String {
    format!("00000000-0000-4000-8000-{tail:0>12}")
}

/// A workspace UUID with a readable tail.
pub fn workspace(tail: &str) -> String {
    format!("11111111-0000-4000-8000-{tail:0>12}")
}
