//! One fixture per arm of both worker-link oneofs, so the round-trip and
//! field-number tests assert against every arm rather than a sample. Included
//! by both codec test binaries through `#[path]`, so the two share one list
//! instead of restating 55 fixtures.
#![allow(dead_code)]

use crate::fixtures::{FINGERPRINT, SESSION, STREAM, channel, hello, session};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::{
    AgentStatusFrame, Binary, CoordWorkerDownstream, CoordWorkerUpstream, EventAck, InputResult,
    RefreshJwt, TerminalInputStatus, TerminalMetadata, TerminalSnapshotRequest,
    TerminalStreamFailureKind, TerminalStreamResult, TerminalStreamStatus, TerminalWritePhase,
    UpdateProgress,
};

pub fn upstream_arms() -> Vec<(&'static str, CoordWorkerUpstream)> {
    vec![
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
                failure_kind: TerminalStreamFailureKind::SessionNotLive,
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
                title: "vim".to_owned(),
                activity_changed: true,
                activity_ts_ms: 1_700_000_000_001,
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
    ]
}

/// Every downstream arm, one fixture each, for the same reason.
pub fn downstream_arms() -> Vec<(&'static str, CoordWorkerDownstream)> {
    vec![
        (
            "hello-ack",
            CoordWorkerDownstream::HelloAck {
                capabilities: vec!["events-v1".to_owned(), "cell-shipping-v2".to_owned()],
                trace_id: None,
            },
        ),
        (
            "ping",
            CoordWorkerDownstream::Ping {
                ts: 1_700_000_000_001,
                trace_id: None,
            },
        ),
        (
            "browser-command",
            CoordWorkerDownstream::BrowserCommand {
                browser_id: "browser-1".to_owned(),
                viewer_id: "viewer-1".to_owned(),
                request_id: "req-6".to_owned(),
                frame: ClientControlFrame::parse(serde_json::json!({
                    "kind": "attach",
                    "session_id": SESSION,
                }))
                .expect("a canonical attach"),
                trace_id: None,
            },
        ),
        (
            "event-ack",
            CoordWorkerDownstream::EventAck(EventAck { client_seq: 42 }),
        ),
        (
            "binary",
            CoordWorkerDownstream::Binary(Binary {
                channel_id: channel(4),
                direction: 1,
                data: b"ls -la\r".to_vec(),
                seq: 0,
            }),
        ),
        (
            "input-request",
            CoordWorkerDownstream::InputRequest(Default::default()),
        ),
        (
            "terminal-stream-state",
            CoordWorkerDownstream::TerminalStreamState(Default::default()),
        ),
        (
            "terminal-snapshot-request",
            CoordWorkerDownstream::TerminalSnapshotRequest(TerminalSnapshotRequest {
                session_id: session(SESSION),
                stream_id: STREAM.to_owned(),
            }),
        ),
        (
            "terminal-pipeline-snapshot",
            CoordWorkerDownstream::TerminalPipelineSnapshot(Default::default()),
        ),
        (
            "terminal-view-relay",
            CoordWorkerDownstream::TerminalViewRelay(Default::default()),
        ),
        (
            "terminal-view-socket-closed",
            CoordWorkerDownstream::TerminalViewSocketClosed(Default::default()),
        ),
        (
            "local-terminal-grant",
            CoordWorkerDownstream::LocalTerminalGrant(Default::default()),
        ),
        (
            "local-terminal-grant-revoke",
            CoordWorkerDownstream::LocalTerminalGrantRevoke(Default::default()),
        ),
        (
            "local-terminal-peer-offer",
            CoordWorkerDownstream::LocalTerminalPeerOffer(Default::default()),
        ),
        (
            "local-terminal-peer-cancel",
            CoordWorkerDownstream::LocalTerminalPeerCancel(Default::default()),
        ),
        (
            "local-attachment-peer-offer",
            CoordWorkerDownstream::LocalAttachmentPeerOffer(Default::default()),
        ),
        (
            "local-attachment-peer-cancel",
            CoordWorkerDownstream::LocalAttachmentPeerCancel(Default::default()),
        ),
        (
            "local-attachment-grant",
            CoordWorkerDownstream::LocalAttachmentGrant(Default::default()),
        ),
        (
            "local-attachment-grant-revoke",
            CoordWorkerDownstream::LocalAttachmentGrantRevoke(Default::default()),
        ),
        (
            "attachment-direct-status-request",
            CoordWorkerDownstream::AttachmentDirectStatusRequest(Default::default()),
        ),
        (
            "attachment-chunk",
            CoordWorkerDownstream::AttachmentChunk(Default::default()),
        ),
        (
            "terminal-input-route-claim",
            CoordWorkerDownstream::TerminalInputRouteClaim(Default::default()),
        ),
        (
            "terminal-transport-probe",
            CoordWorkerDownstream::TerminalTransportProbe(Default::default()),
        ),
        (
            "terminal-direct-retire",
            CoordWorkerDownstream::TerminalDirectRetire(Default::default()),
        ),
        (
            "coord-move-prepare",
            CoordWorkerDownstream::CoordMovePrepare(Default::default()),
        ),
        (
            "coord-move-snapshot-start",
            CoordWorkerDownstream::CoordMoveSnapshotStart(Default::default()),
        ),
        (
            "coord-move-snapshot-chunk",
            CoordWorkerDownstream::CoordMoveSnapshotChunk(Default::default()),
        ),
        (
            "coord-relocate",
            CoordWorkerDownstream::CoordRelocate(Default::default()),
        ),
        (
            "update-broker",
            CoordWorkerDownstream::UpdateBroker(Default::default()),
        ),
        (
            "keeper-update-prepare",
            CoordWorkerDownstream::KeeperUpdatePrepare(Default::default()),
        ),
        (
            "agent-prompt",
            CoordWorkerDownstream::AgentPrompt(Default::default()),
        ),
    ]
}
