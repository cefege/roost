//! A fake worker for the terminal-pipeline sampling tests: a real
//! `WorkerRegistry` and `ScrollbackRelay`, and a `WorkerHandle` whose outbound
//! pipeline requests are recorded and handed to a per-test reply hook.
//!
//! Shared by `terminal_screen_pipeline_snapshot.rs` and
//! `terminal_screen_pipeline_cache.rs`, which are separate crates.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::terminal_screen::pipeline_snapshot::TerminalPipelineDiagnosticTarget;
use roost_coord::terminal_screen::scrollback_relay::ScrollbackRelay;
use roost_coord::terminal_screen::typed_results::TypedWorkerResult;
use roost_proto::{
    DTerminalPipelineSnapshotRequest, TerminalPipelineReason, TerminalPipelineSessionSnapshot,
    TerminalPipelineStage, TerminalPipelineStageSnapshot, WTerminalPipelineSnapshot,
};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

pub const TARGET_WORKER_FP: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const SPOOFED_WORKER_FP: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub type ReplyHook =
    Arc<dyn Fn(&ScrollbackRelay, &DTerminalPipelineSnapshotRequest) -> i64 + Send + Sync>;

/// One coordinator's worker registry and correlation table, plus every
/// pipeline request any fake worker on it was sent.
pub struct FakeWorkers {
    pub relay: ScrollbackRelay,
    pub requests: Arc<Mutex<Vec<DTerminalPipelineSnapshotRequest>>>,
}

impl FakeWorkers {
    pub fn new() -> Self {
        Self {
            relay: ScrollbackRelay::with_clock(Arc::new(WorkerRegistry::new()), Arc::new(|| 1_000)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Registers a generation for `worker_fp` whose socket records each
    /// pipeline request and answers with whatever `reply` returns (the
    /// socket's delivery sequence; 0 is a dropped write).
    pub fn connect(
        &self,
        worker_fp: &str,
        generation: &str,
        ready: bool,
        reply: ReplyHook,
    ) -> Arc<WorkerHandle> {
        let relay = self.relay.clone();
        let requests = Arc::clone(&self.requests);
        let handle = Arc::new(WorkerHandle::new(
            WorkerFp::try_from(worker_fp).unwrap(),
            None,
            generation.to_owned(),
            Default::default(),
            Arc::new(move |frame: CoordWorkerDownstream| {
                let CoordWorkerDownstream::TerminalPipelineSnapshot(request) = frame else {
                    panic!("expected a terminal pipeline snapshot request");
                };
                requests.lock().unwrap().push(request.clone());
                reply(&relay, &request)
            }),
        ));
        if ready {
            assert!(handle.mark_ready());
        }
        self.relay.workers().insert(Arc::clone(&handle));
        handle
    }

    pub fn requests(&self) -> Vec<DTerminalPipelineSnapshotRequest> {
        self.requests.lock().unwrap().clone()
    }
}

/// A socket that takes the frame and leaves the reply to the test.
pub fn accept_only() -> ReplyHook {
    Arc::new(|_, _| 1)
}

/// Settles the pending sample as the named worker's reply frame would.
pub fn resolve_pipeline(
    relay: &ScrollbackRelay,
    reply: WTerminalPipelineSnapshot,
    worker_fp: &str,
) -> bool {
    relay
        .pending()
        .resolve_typed(TypedWorkerResult::PipelineSnapshot(reply), Some(worker_fp))
}

pub fn target(session_id: &str, view_id: &str) -> TerminalPipelineDiagnosticTarget {
    TerminalPipelineDiagnosticTarget::new(session_id, view_id)
}

pub fn request_targets(
    request: &DTerminalPipelineSnapshotRequest,
) -> Vec<TerminalPipelineDiagnosticTarget> {
    request
        .targets
        .iter()
        .map(|target| {
            TerminalPipelineDiagnosticTarget::new(target.session_id.clone(), target.view_id.clone())
        })
        .collect()
}

/// One worker-stream stage per target, as v2's `pipelineReply` builds it.
pub fn pipeline_reply(
    request_id: &str,
    targets: &[TerminalPipelineDiagnosticTarget],
) -> WTerminalPipelineSnapshot {
    WTerminalPipelineSnapshot {
        request_id: request_id.to_owned(),
        sessions: targets
            .iter()
            .map(|target| TerminalPipelineSessionSnapshot {
                session_id: target.session_id.clone(),
                view_id: target.view_id.clone(),
                stages: vec![worker_stream_stage()],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

pub fn worker_stream_stage() -> TerminalPipelineStageSnapshot {
    TerminalPipelineStageSnapshot {
        stage: TerminalPipelineStage::WorkerStream.into(),
        reason: TerminalPipelineReason::None.into(),
        generation: 7,
        stream_id: "stream-7".to_owned(),
        sequence: 11,
        ..Default::default()
    }
}

/// Lets spawned cache work and the fake worker's sends run.
pub async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
