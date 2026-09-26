//! The shared harness for the end-to-end scrollback RPC tests: a real
//! migrated database, one open session on one live worker socket generation, and
//! a log of every downstream frame that worker was sent.
//!
//! Separate from the two test files that use it because `tests/*.rs` are
//! independent crates, so a harness has to live in a module both can include
//! rather than in one of them.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::caller::{Caller, ListenerTrust};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use sqlx::AssertSqlSafe;
use std::time::Duration;

pub const WORKER_FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

/// A coordinator with one live worker and one open session on it.
pub struct Harness {
    pub core: CoordCore,
    root: PathBuf,
    sent: Arc<Mutex<Vec<CoordWorkerDownstream>>>,
    pub session_id: String,
}

impl Harness {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-scrollback-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .expect("an empty database is a self-hosted deployment to create");
        seed(&database, &tenant.dashboard_id).await;
        let services = Arc::new(CoordServices::new(database));
        let sent: Arc<Mutex<Vec<CoordWorkerDownstream>>> = Arc::new(Mutex::new(Vec::new()));
        let handle = WorkerHandle::new(
            WorkerFp::try_from(WORKER_FP).unwrap(),
            None,
            "gen-1".to_owned(),
            Default::default(),
            {
                let sent = Arc::clone(&sent);
                Arc::new(move |frame: CoordWorkerDownstream| {
                    sent.lock()
                        .expect("the sent frame log is not poisoned")
                        .push(frame);
                    0
                })
            },
        );
        assert!(
            handle.mark_ready(),
            "the generation crossed its snapshot barrier"
        );
        services.workers.insert(Arc::new(handle));
        Self {
            core: CoordCore::new(services),
            root,
            sent,
            session_id: session("1"),
        }
    }

    pub fn caller(&self) -> Caller {
        Caller {
            principal: Principal::AccountDevice {
                fingerprint: "browser-fp".to_owned(),
                label: "laptop".to_owned(),
                account_id: "account-1".to_owned(),
            },
            tab_id: Some("tab-1".to_owned()),
            remote_address: Some("127.0.0.1:51000".to_owned()),
            on_host: true,
            listener_trust: ListenerTrust::DirectLoopback,
        }
    }

    pub fn frames(&self) -> Vec<CoordWorkerDownstream> {
        self.sent
            .lock()
            .expect("the sent frame log is not poisoned")
            .clone()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn session(tail: &str) -> String {
    format!("00000000-0000-4000-8000-{tail:0>12}")
}

pub async fn seed(database: &CoordDb, dashboard_id: &str) {
    sqlx::query(AssertSqlSafe(
        "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
         VALUES (?1, 'laptop', 'linux', 0, 0, ?2)",
    ))
    .bind(WORKER_FP)
    .bind(dashboard_id)
    .execute(database.pool())
    .await
    .expect("a worker row");
    sqlx::query(AssertSqlSafe(
        "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
         VALUES (?1, ?2, ?3, 1, 'shell', '/tmp', 'open', 0)",
    ))
    .bind(session("1"))
    .bind(dashboard_id)
    .bind(WORKER_FP)
    .execute(database.pool())
    .await
    .expect("an open session row");
}

/// The control frame of one downstream envelope.
pub fn frame_of(downstream: &CoordWorkerDownstream) -> (&str, &ClientControlFrame, &str, &str) {
    match downstream {
        CoordWorkerDownstream::BrowserCommand {
            browser_id,
            viewer_id,
            request_id,
            frame,
            ..
        } => (browser_id, frame, viewer_id, request_id),
        _ => panic!("a scrollback read only sends browser commands"),
    }
}

/// Wait until the harness has sent at least `count` frames, and hand back the
/// last one. The handler runs on its own task because the send is synchronous
/// and the settle is not.
pub async fn wait_for_frame(harness: &Harness, count: usize) -> CoordWorkerDownstream {
    for _ in 0..2_000 {
        let frames = harness.frames();
        if frames.len() >= count {
            return frames[count - 1].clone();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!(
        "the worker was never sent frame {count}; saw {}",
        harness.frames().len()
    );
}
