//! The coordinator-to-worker direction of the worker link, one variant per arm
//! of the protobuf `CoordWorkerDown` oneof. Called by the link loop for every
//! frame the worker reads, and by `CoordWorkerDownstream::parse` for the JSON
//! form. Depends on `wire::control` for the relayed browser command and on
//! `coord_worker::payloads` for the frame records.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::kind_of;
use super::payloads::{
    Binary, DAgentPrompt, DAttachmentChunk, DAttachmentDirectStatusRequest, DCoordMovePrepare,
    DCoordMoveSnapshotChunk, DCoordMoveSnapshotStart, DCoordRelocate, DInputRequest,
    DKeeperUpdatePrepare, DLocalAttachmentGrant, DLocalAttachmentGrantRevoke,
    DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer, DLocalTerminalGrant,
    DLocalTerminalGrantRevoke, DLocalTerminalPeerCancel, DLocalTerminalPeerOffer,
    DTerminalDirectRetire, DTerminalInputRouteClaim, DTerminalPipelineSnapshotRequest,
    DTerminalStreamState, DTerminalTransportProbe, DTerminalViewRelay, DTerminalViewSocketClosed,
    DUpdateBroker, EventAck, TerminalSnapshotRequest,
};
use crate::validate::nonnegative;
use crate::wire::brand::TraceId;
use crate::wire::control::ClientControlFrame;
use crate::{ProtocolError, ProtocolResult};

/// A frame travelling from the coordinator to the worker.
///
/// The `browser-command` arm relays a whole control frame, the rest carry a
/// few scalars or a generated message. Boxing the relayed frame would add an
/// allocation to every keystroke path a browser command takes, which is the
/// opposite of what the size difference is warning about.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CoordWorkerDownstream {
    /// The immediate reply to `hello`, and the barrier that says the link is
    /// ready to carry commands. `capabilities` is what the worker negotiates
    /// against; field numbers 1 and 2 are reserved in the proto, so it is 3.
    #[serde(rename = "hello-ack")]
    HelloAck {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "ping")]
    Ping {
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// A browser's control frame, routed here for execution at the worker.
    /// `browser_id` and `viewer_id` are opaque to the worker — it does not
    /// learn who is watching — and are carried so multi-viewer presence needs
    /// no second channel later. The worker must echo `request_id` in whatever
    /// it replies, which is how the reply finds its way home.
    #[serde(rename = "browser-command")]
    BrowserCommand {
        browser_id: String,
        viewer_id: String,
        request_id: String,
        frame: ClientControlFrame,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The exact sequence of a settled durable insert (or unique-index dedup).
    /// Without this arm the worker's replay barrier cannot be released past
    /// `replay`, because nothing on the wire could say which event landed.
    #[serde(rename = "event-ack")]
    EventAck(EventAck),
    /// PTY bytes going the other way, on the viewer's own channel id.
    #[serde(rename = "binary")]
    Binary(Binary),
    /// A bounded terminal-control request. `budget_ms` is RELATIVE time, never
    /// an instant: the two hosts' clocks may drift arbitrarily.
    #[serde(rename = "input-request")]
    InputRequest(DInputRequest),
    /// The coordinator's already-aggregated geometry and one stream
    /// generation; no per-viewer membership crosses this boundary.
    #[serde(rename = "terminal-stream-state")]
    TerminalStreamState(DTerminalStreamState),
    /// Fire-and-forget repair request. Repeating it replaces any partial
    /// same-stream cursor with a fresh snapshot id and full baseline.
    #[serde(rename = "terminal-snapshot-request")]
    TerminalSnapshotRequest(TerminalSnapshotRequest),
    /// The coordinator's choice of view-correlated pipeline-sample targets.
    #[serde(rename = "terminal-pipeline-snapshot")]
    TerminalPipelineSnapshot(DTerminalPipelineSnapshotRequest),
    /// A relay to a worker that owns its own terminal views. The coordinator
    /// has already authenticated and authorized the browser socket; it
    /// forwards the command verbatim and never interprets the membership.
    #[serde(rename = "terminal-view-relay")]
    TerminalViewRelay(DTerminalViewRelay),
    #[serde(rename = "terminal-view-socket-closed")]
    TerminalViewSocketClosed(DTerminalViewSocketClosed),
    /// Authorization for one browser to talk to this worker's direct terminal
    /// carrier. Only the secret's digest crosses the wire; the secret itself
    /// goes to the browser over the coordinator's authenticated RPC.
    #[serde(rename = "local-terminal-grant")]
    LocalTerminalGrant(DLocalTerminalGrant),
    #[serde(rename = "local-terminal-grant-revoke")]
    LocalTerminalGrantRevoke(DLocalTerminalGrantRevoke),
    #[serde(rename = "local-terminal-peer-offer")]
    LocalTerminalPeerOffer(DLocalTerminalPeerOffer),
    #[serde(rename = "local-terminal-peer-cancel")]
    LocalTerminalPeerCancel(DLocalTerminalPeerCancel),
    #[serde(rename = "local-attachment-peer-offer")]
    LocalAttachmentPeerOffer(DLocalAttachmentPeerOffer),
    #[serde(rename = "local-attachment-peer-cancel")]
    LocalAttachmentPeerCancel(DLocalAttachmentPeerCancel),
    #[serde(rename = "local-attachment-grant")]
    LocalAttachmentGrant(DLocalAttachmentGrant),
    #[serde(rename = "local-attachment-grant-revoke")]
    LocalAttachmentGrantRevoke(DLocalAttachmentGrantRevoke),
    #[serde(rename = "attachment-direct-status-request")]
    AttachmentDirectStatusRequest(DAttachmentDirectStatusRequest),
    #[serde(rename = "attachment-chunk")]
    AttachmentChunk(DAttachmentChunk),
    /// The claim a coordinator makes before it owns a session's input lane,
    /// carrying the revision and worker epoch that fence a stale claim.
    #[serde(rename = "terminal-input-route-claim")]
    TerminalInputRouteClaim(DTerminalInputRouteClaim),
    #[serde(rename = "terminal-transport-probe")]
    TerminalTransportProbe(DTerminalTransportProbe),
    /// A dead direct connection's worker epoch, so every callback still in
    /// flight for it is identity-fenced rather than delivered.
    #[serde(rename = "terminal-direct-retire")]
    TerminalDirectRetire(DTerminalDirectRetire),
    /// The coordinator move handshake. Authenticated coordinator control; the
    /// worker validates the detached vendor signature before acting.
    #[serde(rename = "coord-move-prepare")]
    CoordMovePrepare(DCoordMovePrepare),
    #[serde(rename = "coord-move-snapshot-start")]
    CoordMoveSnapshotStart(DCoordMoveSnapshotStart),
    #[serde(rename = "coord-move-snapshot-chunk")]
    CoordMoveSnapshotChunk(DCoordMoveSnapshotChunk),
    #[serde(rename = "coord-relocate")]
    CoordRelocate(DCoordRelocate),
    /// The out-of-job update broker. Runs only under the coordinator's
    /// exclusive channel-creation drain.
    #[serde(rename = "update-broker")]
    UpdateBroker(DUpdateBroker),
    /// A journaled keeper update, carrying its complete immutable envelope.
    #[serde(rename = "keeper-update-prepare")]
    KeeperUpdatePrepare(DKeeperUpdatePrepare),
    /// An agent prompt, gated on the status epoch, occupant and revision it
    /// was composed against so a fenced occupant's prompt is not delivered.
    #[serde(rename = "agent-prompt")]
    AgentPrompt(DAgentPrompt),
}

impl CoordWorkerDownstream {
    /// The wire spelling of this frame's discriminant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::HelloAck { .. } => "hello-ack",
            Self::Ping { .. } => "ping",
            Self::BrowserCommand { .. } => "browser-command",
            Self::EventAck(_) => "event-ack",
            Self::Binary(_) => "binary",
            Self::InputRequest(_) => "input-request",
            Self::TerminalStreamState(_) => "terminal-stream-state",
            Self::TerminalSnapshotRequest(_) => "terminal-snapshot-request",
            Self::TerminalPipelineSnapshot(_) => "terminal-pipeline-snapshot",
            Self::TerminalViewRelay(_) => "terminal-view-relay",
            Self::TerminalViewSocketClosed(_) => "terminal-view-socket-closed",
            Self::LocalTerminalGrant(_) => "local-terminal-grant",
            Self::LocalTerminalGrantRevoke(_) => "local-terminal-grant-revoke",
            Self::LocalTerminalPeerOffer(_) => "local-terminal-peer-offer",
            Self::LocalTerminalPeerCancel(_) => "local-terminal-peer-cancel",
            Self::LocalAttachmentPeerOffer(_) => "local-attachment-peer-offer",
            Self::LocalAttachmentPeerCancel(_) => "local-attachment-peer-cancel",
            Self::LocalAttachmentGrant(_) => "local-attachment-grant",
            Self::LocalAttachmentGrantRevoke(_) => "local-attachment-grant-revoke",
            Self::AttachmentDirectStatusRequest(_) => "attachment-direct-status-request",
            Self::AttachmentChunk(_) => "attachment-chunk",
            Self::TerminalInputRouteClaim(_) => "terminal-input-route-claim",
            Self::TerminalTransportProbe(_) => "terminal-transport-probe",
            Self::TerminalDirectRetire(_) => "terminal-direct-retire",
            Self::CoordMovePrepare(_) => "coord-move-prepare",
            Self::CoordMoveSnapshotStart(_) => "coord-move-snapshot-start",
            Self::CoordMoveSnapshotChunk(_) => "coord-move-snapshot-chunk",
            Self::CoordRelocate(_) => "coord-relocate",
            Self::UpdateBroker(_) => "update-broker",
            Self::KeeperUpdatePrepare(_) => "keeper-update-prepare",
            Self::AgentPrompt(_) => "agent-prompt",
        }
    }

    /// Decode and check one downstream frame. `value` is the already-decoded
    /// JSON.
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        // The wrapped control frame goes through its own admission path first,
        // strict keys included: relaying a browser command is not permission to
        // relay a frame the browser could not have sent the worker directly.
        if let (Some("browser-command"), Some(frame)) = (kind_of(&value), value.get("frame")) {
            ClientControlFrame::parse(frame.clone())?;
        }
        let frame: Self = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("coord_worker_downstream", error.to_string()))?;
        frame.check()?;
        Ok(frame)
    }

    /// The rules a constructed value can be held to, beyond the shapes their
    /// own types already enforce.
    pub fn check(&self) -> ProtocolResult<()> {
        match self {
            Self::Ping { ts, .. } => nonnegative("ping.ts", *ts),
            Self::BrowserCommand { frame, .. } => frame.check(),
            Self::Binary(binary) => binary.check(),
            _ => Ok(()),
        }
    }
}
