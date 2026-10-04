//! A booted coordinator over a scratch database for the direct-terminal RPC
//! and worker-link tests: a self-hosted tenant, a local and a remote worker
//! row, a browser caller on one tab, and the resolved peer config.
//! Ports the database and handler setup of `apps/coord/tests/local-terminal-grant.test.ts`.

// Compiled into more than one test binary; each uses a subset.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::services::CoordServices;
use roost_coord::terminal_direct::grant_rpc::handle_sessions_grant_local_terminal;
use roost_host::{CoordConfig, CoordConfigInput};
use roost_proto::{
    DLocalTerminalGrant, SessionsGrantLocalTerminalRequest, SessionsGrantLocalTerminalResponse,
};
use tokio::task::JoinHandle;

use super::terminal_direct_support::{DEVICE_FP, TestWorker, settle_until};

pub const LOCAL_WORKER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const REMOTE_WORKER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const LOCAL_TAB: &str = "local-tab";
pub const LOCAL_EPOCH: &str = "worker-epoch-local";
pub const REMOTE_EPOCH: &str = "worker-epoch-remote";
pub const STUN_URL: &str = "stun:stun.example.test:3478";

/// The coordinator a handler receives, and the browser calling it.
pub struct DirectCore {
    pub core: CoordCore,
    pub caller: Caller,
    pub dashboard_id: String,
    root: PathBuf,
    next_channel: std::sync::atomic::AtomicI64,
}

impl DirectCore {
    /// A booted coordinator whose config offers (or withholds) the peer carrier.
    pub async fn new(label: &str, peer_enabled: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-terminal-direct-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a database");
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .expect("a self-hosted tenant");
        for fp in [LOCAL_WORKER, REMOTE_WORKER] {
            sqlx::query(
                "INSERT INTO workers (dashboard_id, fp, label, os, git_sha, host_metrics_json, \
                                      registered_at_ms, last_seen_ms, reachable_addr) \
                 VALUES (?1, ?2, 'direct-worker', 'linux', NULL, NULL, 1000, 1000, NULL)",
            )
            .bind(&tenant.dashboard_id)
            .bind(fp)
            .execute(database.pool())
            .await
            .expect("a worker row");
        }
        let config = CoordConfig::parse(CoordConfigInput {
            db_path: Some(root.join("coord.db")),
            authorized_keys_path: Some(root.join("authorized_keys.roost")),
            log_dir: Some(root.clone()),
            terminal_peer_enabled: Some(peer_enabled),
            terminal_peer_stun_urls: Some(vec![STUN_URL.to_owned()]),
            ..CoordConfigInput::default()
        })
        .expect("a coordinator config");
        let services = CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant.clone()),
                config: Some(Arc::new(config)),
                process_epoch: "terminal-direct-fixture".to_owned(),
                boot_ms: 1_000,
            },
        );
        let caller = Caller {
            principal: Principal::AccountDevice {
                fingerprint: DEVICE_FP.to_owned(),
                label: "Terminal browser".to_owned(),
                account_id: tenant.account_id.clone(),
            },
            tab_id: Some(LOCAL_TAB.to_owned()),
            remote_address: None,
            on_host: false,
            listener_trust: ListenerTrust::DirectLoopback,
        };
        Self {
            core: CoordCore::new(Arc::new(services)),
            caller,
            dashboard_id: tenant.dashboard_id,
            root,
            next_channel: std::sync::atomic::AtomicI64::new(1),
        }
    }

    /// One session row on `worker_fp`, open or closed.
    pub async fn insert_session(&self, worker_fp: &str, status: &str) -> String {
        let channel = self
            .next_channel
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("00000000-0000-4000-8000-{channel:012}");
        sqlx::query(
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id, \
                                   status, created_at, closed_at, custom_title, spawn_cwd) \
             VALUES (?1, ?2, ?3, ?4, 'shell', '/tmp', NULL, ?5, 1000, NULL, NULL, '/tmp')",
        )
        .bind(&id)
        .bind(&self.dashboard_id)
        .bind(worker_fp)
        .bind(channel)
        .bind(status)
        .execute(self.core.services.db.pool())
        .await
        .expect("a session row");
        id
    }

    /// The caller, claiming another tab (or none).
    pub fn caller_on(&self, tab_id: Option<&str>) -> Caller {
        Caller {
            tab_id: tab_id.map(str::to_owned),
            ..self.caller.clone()
        }
    }

    /// Run the grant handler on its own task, as the Connect server would.
    pub fn request_grant(
        &self,
        caller: Caller,
        worker_fp: &str,
        tab_id: &str,
        session_ids: Vec<String>,
    ) -> JoinHandle<Result<SessionsGrantLocalTerminalResponse, connectrpc::ConnectError>> {
        let core = self.core.clone();
        let request = SessionsGrantLocalTerminalRequest {
            session_ids,
            worker_fp: worker_fp.to_owned(),
            tab_id: tab_id.to_owned(),
            ..Default::default()
        };
        tokio::spawn(async move {
            handle_sessions_grant_local_terminal(&core, &caller, request)
                .await
                .map(|response| response.body)
        })
    }

    /// Wait for the next grant install on `worker`.
    pub async fn next_install(&self, worker: &TestWorker, before: usize) -> DLocalTerminalGrant {
        settle_until(|| worker.grants().len() > before).await;
        worker.grants()[before].clone()
    }

    /// Acknowledge an install as the worker's `rpc-ok`.
    pub fn ack(&self, worker: &TestWorker, frame: &DLocalTerminalGrant) {
        let worker_fp = worker.handle.worker_fp.as_str();
        let pending = self.core.services.scrollback.pending();
        assert!(pending.resolve(&frame.request_id, serde_json::json!({}), Some(worker_fp)));
    }

    /// A grant the worker acknowledged, from request to response.
    pub async fn grant_with_ack(
        &self,
        worker: &TestWorker,
        session_ids: Vec<String>,
    ) -> SessionsGrantLocalTerminalResponse {
        let before = worker.grants().len();
        let worker_fp = worker.handle.worker_fp.as_str().to_owned();
        let response = self.request_grant(self.caller.clone(), &worker_fp, LOCAL_TAB, session_ids);
        let frame = self.next_install(worker, before).await;
        self.ack(worker, &frame);
        response
            .await
            .expect("the handler task")
            .expect("a granted terminal")
    }
}

impl Drop for DirectCore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
