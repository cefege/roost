//! One fixture per worker-link oneof arm, keeping round-trip and field-number tests exhaustive.
//! Both codec test binaries include this through `#[path]` so the fixtures have one owner.
//! Shared fixtures prevent either codec test binary from missing an arm.
#![allow(dead_code)]

use super::fixtures::{FINGERPRINT, SESSION, STREAM, channel, hello, session};
use roost_proto as proto;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::{
    AgentStatusFrame, Binary, CoordWorkerDownstream, CoordWorkerUpstream, EventAck, InputResult,
    RefreshJwt, TerminalInputStatus, TerminalMetadata, TerminalSnapshotRequest,
    TerminalStreamFailureKind, TerminalStreamResult, TerminalStreamStatus, TerminalWritePhase,
    UpdateProgress,
};

#[path = "live_metrics_arms.rs"]
mod live_metrics_arms;

#[path = "downstream_arms.rs"]
mod downstream;
pub use downstream::downstream_arms;

pub fn upstream_arms() -> Vec<(&'static str, CoordWorkerUpstream)> {
    let mut arms = vec![
        ("hello", hello()),
        (
            "pong",
            CoordWorkerUpstream::Pong {
                ts: 1_700_000_000_001,
                trace_id: None,
            },
        ),
        (
            "event",
            CoordWorkerUpstream::Event {
                event: serde_json::from_value(serde_json::json!({
                    "kind": "opened",
                    "session_id": SESSION,
                    "worker_fp": FINGERPRINT,
                    "channel": 3,
                    "session_kind": "shell",
                    "cwd": "/srv",
                    "ts": 1_700_000_000_001i64,
                }))
                .expect("a canonical opened event"),
                client_seq: 7,
                trace_id: None,
            },
        ),
        (
            "rpc-ok",
            CoordWorkerUpstream::RpcOk {
                request_id: "req-1".to_owned(),
                data: serde_json::json!({ "ok": true, "rows": 3 }),
                trace_id: None,
            },
        ),
        (
            "rpc-error",
            CoordWorkerUpstream::RpcError {
                request_id: "req-2".to_owned(),
                message: "no such session".to_owned(),
                trace_id: None,
            },
        ),
        (
            "binary",
            CoordWorkerUpstream::Binary(Binary {
                channel_id: channel(4),
                direction: 0,
                data: b"hello\r\n".to_vec(),
                seq: 91,
            }),
        ),
        (
            "refresh-jwt",
            CoordWorkerUpstream::RefreshJwt(RefreshJwt {
                jwt: "a.b.c".to_owned(),
            }),
        ),
        (
            "cell-grid",
            CoordWorkerUpstream::CellGrid(Default::default()),
        ),
        (
            "cell-grid-chunk",
            CoordWorkerUpstream::CellGridChunk(Default::default()),
        ),
        (
            "input-result",
            CoordWorkerUpstream::InputResult(InputResult {
                request_id: "req-3".to_owned(),
                session_id: session(SESSION),
                input_seq: 12,
                status: TerminalInputStatus::Rejected,
                written_bytes: 0,
                reason: "the budget expired".to_owned(),
                phase: TerminalWritePhase::PreWrite,
            }),
        ),
        (
            "terminal-stream-result",
            CoordWorkerUpstream::TerminalStreamResult(TerminalStreamResult {
                request_id: "req-4".to_owned(),
                session_id: session(SESSION),
                stream_id: STREAM.to_owned(),
                enabled: true,
                status: TerminalStreamStatus::Committed,
                channel_resize_seq: 5,
                effective_cols: 120,
                effective_rows: 40,
                resized: true,
                reason: String::new(),
                phase: TerminalWritePhase::Written,
                failure_kind: Some(TerminalStreamFailureKind::SessionNotLive),
            }),
        ),
        (
            "agent-status",
            CoordWorkerUpstream::AgentStatus(AgentStatusFrame {
                status: serde_json::from_value(serde_json::json!({
                    "session_id": SESSION,
                    "agent_id": "claude",
                    "state": "working",
                    "message": "reading",
                    "revision": 4,
                    "completed_revision": 1,
                    "updated_at": 1_700_000_000_001i64,
                    "status_epoch": "6f1a0b1e-6c1f-4a3a-9f0e-2b7d5c8e4a11",
                    "occupant_id": "6f1a0b1e-6c1f-4a3a-9f0e-2b7d5c8e4a11",
                    "source": "integration",
                    "occupant_exited": false,
                    "active": true,
                }))
                .expect("a canonical agent status"),
            }),
        ),
        (
            "update-progress",
            CoordWorkerUpstream::UpdateProgress(UpdateProgress {
                request_id: "req-5".to_owned(),
                job_id: "job-9".to_owned(),
                sequence: 3,
                phase: "download".to_owned(),
                message: "receiving".to_owned(),
                terminal: false,
                success: false,
                error: String::new(),
            }),
        ),
        (
            "terminal-metadata",
            CoordWorkerUpstream::TerminalMetadata(TerminalMetadata {
                channel_id: channel(4),
                title_changed: true,
                program_status: None,
                title: "vim".to_owned(),
                activity_changed: true,
                activity_ts_ms: 1_700_000_000_001,
                clipboard_changed: true,
                clipboard: "copied text".to_owned(),
                command_finished: true,
                command_exit_code: Some(2),
                command_duration_ms: 12_000,
                bell: true,
                progress: None,
                notifications: Vec::new(),
                user_vars_changed: false,
                user_vars: Vec::new(),
            }),
        ),
        (
            "terminal-view-state",
            CoordWorkerUpstream::TerminalViewState(Default::default()),
        ),
        (
            "terminal-view-projection",
            CoordWorkerUpstream::TerminalViewProjection(Default::default()),
        ),
        (
            "local-terminal-peer-answer",
            CoordWorkerUpstream::LocalTerminalPeerAnswer(Default::default()),
        ),
        (
            "local-terminal-peer-error",
            CoordWorkerUpstream::LocalTerminalPeerError(Default::default()),
        ),
        (
            "local-attachment-peer-answer",
            CoordWorkerUpstream::LocalAttachmentPeerAnswer(Default::default()),
        ),
        (
            "local-attachment-peer-error",
            CoordWorkerUpstream::LocalAttachmentPeerError(Default::default()),
        ),
        (
            "attachment-direct-status",
            CoordWorkerUpstream::AttachmentDirectStatus(Default::default()),
        ),
        (
            "terminal-input-route-result",
            CoordWorkerUpstream::TerminalInputRouteResult(Default::default()),
        ),
        (
            "terminal-transport-probe-result",
            CoordWorkerUpstream::TerminalTransportProbeResult(Default::default()),
        ),
        (
            "terminal-pipeline-snapshot",
            CoordWorkerUpstream::TerminalPipelineSnapshot(Default::default()),
        ),
        (
            "omp-host-service-request",
            CoordWorkerUpstream::OmpHostServiceRequest(proto::WOmpHostServiceRequest {
                conversation_id: "c-1".to_owned(),
                service_id: "svc-1".to_owned(),
                origin_request_id: "req-ui-1".to_owned(),
                client_id: "device:view-1".to_owned(),
                view_id: "view-1".to_owned(),
                transport_id: "nonce-1".to_owned(),
                kind: "copy_text".to_owned(),
                value: "clipboard text".to_owned(),
                cancelled: false,
                ..Default::default()
            }),
        ),
        (
            "omp-ui-action-receipt",
            CoordWorkerUpstream::OmpUiActionReceipt(proto::WOmpUiActionReceipt {
                conversation_id: "c-1".to_owned(),
                origin_request_id: "req-ui-2".to_owned(),
                client_id: "device:view-1".to_owned(),
                view_id: "view-1".to_owned(),
                transport_id: "nonce-1".to_owned(),
                error: String::new(),
                ..Default::default()
            }),
        ),
    ];
    arms.extend(live_metrics_arms::upstream_arms());
    arms
}
