//! The grant-owner harness: one registry with two ready workers, the
//! production pending-RPC table the owner correlates ACKs through, and the
//! helpers that start a grant, observe its install, and acknowledge it.
//! Ports the fixture half of `apps/coord/tests/terminal/direct/terminal-grant-owner.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_coord::coord_core::worker_handle::WorkerRegistry;
use roost_coord::terminal_direct::grant_owner::TerminalGrantOwner;
use roost_coord::terminal_direct::grant_refresh::PendingTerminalGrant;
use roost_coord::terminal_direct::grant_state::{
    TerminalGrantAuthorization, TerminalGrantRequest, TerminalGrantResult,
};
use roost_coord::terminal_screen::pending_rpcs::PendingRpcs;
use roost_proto::DLocalTerminalGrant;

use super::terminal_direct_support::{
    TestWorker, WORKER_A, WORKER_B, allow_all, grant_request, install_worker, settle_until,
};

pub struct Harness {
    pub registry: Arc<WorkerRegistry>,
    pub pending: Arc<PendingRpcs>,
    pub owner: Arc<TerminalGrantOwner>,
    pub a: TestWorker,
    pub b: TestWorker,
}

impl Harness {
    pub fn new() -> Self {
        let registry = Arc::new(WorkerRegistry::new());
        let pending = Arc::new(PendingRpcs::new());
        let owner = TerminalGrantOwner::new(Arc::clone(&registry), Arc::clone(&pending));
        let a = install_worker(&registry, WORKER_A, Some("epoch-a"), &[]);
        let b = install_worker(&registry, WORKER_B, Some("epoch-b"), &[]);
        Self {
            registry,
            pending,
            owner,
            a,
            b,
        }
    }

    pub fn ack(&self, worker: &TestWorker, frame: &DLocalTerminalGrant) {
        let worker_fp = worker.handle.worker_fp.as_str();
        assert!(
            self.pending
                .resolve(&frame.request_id, serde_json::json!({}), Some(worker_fp))
        );
    }

    /// Start a grant and wait until its install reaches `worker`.
    pub async fn start(
        &self,
        worker: &TestWorker,
        request: TerminalGrantRequest,
    ) -> (PendingTerminalGrant, DLocalTerminalGrant) {
        let before = worker.grants().len();
        let pending = self.owner.grant(request);
        settle_until(|| worker.grants().len() > before).await;
        (pending, worker.grants()[before].clone())
    }

    pub async fn grant_with_ack(
        &self,
        worker: &TestWorker,
        sessions: &[&str],
    ) -> TerminalGrantResult {
        let worker_fp = worker.handle.worker_fp.as_str().to_owned();
        let (pending, frame) = self
            .start(worker, grant_request(&worker_fp, sessions, allow_all()))
            .await;
        self.ack(worker, &frame);
        pending.result().await.unwrap()
    }
}

pub fn counting(calls: &Arc<AtomicUsize>) -> TerminalGrantAuthorization {
    let calls = Arc::clone(calls);
    Arc::new(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    })
}

pub fn sessions_of(result: &TerminalGrantResult) -> Vec<&str> {
    result
        .lease
        .session_ids
        .iter()
        .map(String::as_str)
        .collect()
}
