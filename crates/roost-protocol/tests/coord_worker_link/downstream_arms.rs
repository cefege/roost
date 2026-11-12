//! Downstream worker-link fixtures shared by both codec regression suites.
//! Each fixture exercises the protobuf oneof and its actual wire fields.
//! Imports the parent fixture vocabulary to keep session identity consistent.

use super::*;
/// Every downstream arm, one fixture each, for the same reason.
pub fn downstream_arms() -> Vec<(&'static str, CoordWorkerDownstream)> {
    let mut arms = vec![
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
        (
            "omp-ui",
            CoordWorkerDownstream::OmpUi(proto::DOmpUi {
                conversation_id: "c-1".to_owned(),
                request_id: "req-ui-3".to_owned(),
                action_json: r#"{"method":"open","params":{"cols":80,"rows":24}}"#.to_owned(),
                cwd: "/work".to_owned(),
                acp_session_id: "session-1".to_owned(),
                client_id: "device:view-1".to_owned(),
                view_id: "view-1".to_owned(),
                tab_id: "tab-1".to_owned(),
                transport_id: "nonce-1".to_owned(),
                receipt: true,
                ..Default::default()
            }),
        ),
    ];
    arms.extend(live_metrics_arms::downstream_arms());
    arms
}
