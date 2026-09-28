// Fake downstream owners and a fake link for the `link_downstream*` suites. An
// integration test is its own crate and `expect` is denied outside
// `#[cfg(test)]`, so the exemption lives here once.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

//! Every fake records into ONE ordered call log, shared with the fake link, so a
//! suite can assert the order v2 calls its dependencies in, not just that each
//! was called. An owner's future either answers now, panics, or holds on a
//! shared gate the test opens one permit at a time.

pub mod live;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_proto::DAgentPrompt;
use roost_proto::DKeeperUpdatePrepare;
use roost_proto::{
    AttachmentTransferStatus, DAttachmentChunk, DAttachmentDirectStatusRequest,
    DLocalAttachmentGrant,
};
use roost_proto::{
    DInputRequest, DLocalTerminalGrant, DTerminalInputRouteClaim, DTerminalPipelineSnapshotRequest,
    DTerminalStreamState, DTerminalViewRelay, TerminalInputRouteResult, WTerminalPipelineSnapshot,
};
use roost_protocol::wire::brand::{ChannelId, SessionId};
use roost_protocol::wire::coord_worker::{
    CoordWorkerUpstream, InputResult, TerminalInputStatus, TerminalSnapshotRequest,
    TerminalStreamResult, TerminalStreamStatus, TerminalWritePhase,
};
use roost_worker::attachments::upload::RelayChunkOutcome;
use roost_worker::browser_commands::Command;
use roost_worker::link_ports::AgentPromptPort;
use roost_worker::link_ports::AttachmentLinkPort;
use roost_worker::link_ports::KeeperUpdatePort;
use roost_worker::link_ports::{
    DownstreamOwners, LinkLifecyclePort, LinkPipelineState, LocalTerminalGrantPort,
    TerminalInputPort, TerminalPipelinePort, TerminalStreamPort, TerminalViewPort,
};
use roost_worker::runtime::downstream::DownstreamLink;
use roost_worker::uplink::{LinkFence, OwnerFuture, RequestBudget, UplinkReceiver};
use tokio::sync::Semaphore;

pub const SESSION: &str = "00000000-0000-4000-8000-00000000beef";
pub const WORKER_EPOCH: &str = "11111111-1111-4111-8111-111111111111";

/// The ordered record every fake appends to.
#[derive(Debug, Clone, Default)]
pub struct CallLog(Arc<Mutex<Vec<String>>>);

impl CallLog {
    pub fn push(&self, call: impl Into<String>) {
        self.0.lock().unwrap().push(call.into());
    }
    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    pub fn count(&self, prefix: &str) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.starts_with(prefix))
            .count()
    }
}

/// How an owner's future ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerMode {
    Answer,
    Panic,
    /// Waits for one permit of the shared gate, then answers.
    Hold,
}

#[derive(Debug, Clone)]
pub struct Fakes {
    pub log: CallLog,
    pub mode: OwnerMode,
    pub gate: Arc<Semaphore>,
    /// `claim_route`'s answer: a route result, or `None` (a busy route).
    pub claim_accepts: bool,
}

impl Fakes {
    pub fn new(mode: OwnerMode) -> Self {
        Self {
            log: CallLog::default(),
            mode,
            gate: Arc::new(Semaphore::new(0)),
            claim_accepts: true,
        }
    }

    pub fn owners(&self) -> DownstreamOwners {
        let fakes = Arc::new(self.clone());
        DownstreamOwners {
            input: Arc::clone(&fakes) as Arc<dyn TerminalInputPort>,
            stream: Arc::clone(&fakes) as Arc<dyn TerminalStreamPort>,
            pipeline: Arc::clone(&fakes) as Arc<dyn TerminalPipelinePort>,
            view: Arc::clone(&fakes) as Arc<dyn TerminalViewPort>,
            local_terminal: Arc::clone(&fakes) as Arc<dyn LocalTerminalGrantPort>,
            agent_prompt: Arc::clone(&fakes) as Arc<dyn AgentPromptPort>,
            attachments: Arc::clone(&fakes) as Arc<dyn AttachmentLinkPort>,
            keeper_update: Arc::clone(&fakes) as Arc<dyn KeeperUpdatePort>,
            lifecycle: fakes as Arc<dyn LinkLifecyclePort>,
            direct: None,
            attachment_peers: None,
        }
    }

    fn settle<T: Send + 'static>(&self, answer: T) -> OwnerFuture<T> {
        let mode = self.mode;
        let gate = Arc::clone(&self.gate);
        Box::pin(async move {
            match mode {
                OwnerMode::Answer => {}
                OwnerMode::Panic => panic!("the fake owner failed on purpose"),
                OwnerMode::Hold => gate.acquire().await.unwrap().forget(),
            }
            answer
        })
    }
}

impl KeeperUpdatePort for Fakes {
    fn prepare(
        &self,
        request: DKeeperUpdatePrepare,
    ) -> OwnerFuture<Result<serde_json::Value, String>> {
        self.log
            .push(format!("keeper_update.prepare:{}", request.request_id));
        let answer = if request.maintenance {
            Ok(serde_json::json!({ "outcome": "shutdown" }))
        } else {
            Err("journaled keeper update request is malformed".to_owned())
        };
        self.settle(answer)
    }
}

impl AgentPromptPort for Fakes {
    fn write_prompt(
        &self,
        request: DAgentPrompt,
        _: RequestBudget,
        _: LinkFence,
    ) -> OwnerFuture<Option<InputResult>> {
        self.log
            .push(format!("agent_prompt.write_prompt:{}", request.request_id));
        // The failure carries the prompt text, so a secrecy test proves the
        // dispatch never lets an owner's own failure reach a log or a reply.
        if self.mode == OwnerMode::Panic {
            let text = request.text;
            return Box::pin(async move { panic!("{text}") });
        }
        self.settle(Some(InputResult {
            request_id: request.request_id,
            session_id: SessionId::try_from(request.session_id.as_str()).unwrap(),
            input_seq: request.input_seq,
            status: TerminalInputStatus::Accepted,
            written_bytes: u32::try_from(request.text.len() + 1).unwrap(),
            reason: String::new(),
            phase: TerminalWritePhase::Written,
        }))
    }
}

impl TerminalInputPort for Fakes {
    fn write_input(
        &self,
        request: DInputRequest,
        _: RequestBudget,
        _: LinkFence,
    ) -> OwnerFuture<Option<InputResult>> {
        self.log
            .push(format!("input.write_input:{}", request.request_id));
        self.settle(Some(InputResult {
            request_id: request.request_id,
            session_id: SessionId::try_from(request.session_id.as_str()).unwrap(),
            input_seq: request.input_seq,
            status: TerminalInputStatus::Accepted,
            written_bytes: u32::try_from(request.data.len()).unwrap(),
            reason: String::new(),
            phase: TerminalWritePhase::Written,
        }))
    }
    fn write_binary(&self, channel_id: ChannelId, bytes: Vec<u8>) {
        self.log
            .push(format!("input.write_binary:{channel_id}:{}", bytes.len()));
    }
    fn claim_route(
        &self,
        request: DTerminalInputRouteClaim,
        _: RequestBudget,
        _: LinkFence,
    ) -> OwnerFuture<Option<TerminalInputRouteResult>> {
        self.log
            .push(format!("input.claim_route:{}", request.request_id));
        let result = self.claim_accepts.then(|| TerminalInputRouteResult {
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
            revision: request.revision,
            accepted: true,
            latest_revision: request.revision,
            input_route_epoch: "route-epoch".to_owned(),
            worker_epoch: WORKER_EPOCH.to_owned(),
            ..Default::default()
        });
        self.settle(result)
    }
    fn retire_connection(&self, socket_id: &str) {
        self.log
            .push(format!("input.retire_connection:{socket_id}"));
    }
}

impl TerminalStreamPort for Fakes {
    fn apply_stream_state(
        &self,
        request: DTerminalStreamState,
        _: RequestBudget,
        _: LinkFence,
    ) -> OwnerFuture<Option<TerminalStreamResult>> {
        self.log
            .push(format!("stream.apply:{}", request.request_id));
        self.settle(Some(TerminalStreamResult {
            request_id: request.request_id,
            session_id: SessionId::try_from(request.session_id.as_str()).unwrap(),
            stream_id: request.stream_id,
            enabled: request.enabled,
            status: TerminalStreamStatus::Committed,
            channel_resize_seq: 3,
            effective_cols: request.cols,
            effective_rows: request.rows,
            resized: true,
            reason: String::new(),
            phase: TerminalWritePhase::Written,
            failure_kind: None,
        }))
    }
    fn request_snapshot(&self, request: TerminalSnapshotRequest) {
        self.log
            .push(format!("stream.request_snapshot:{}", request.stream_id));
    }
}

impl TerminalPipelinePort for Fakes {
    fn pipeline_snapshot(
        &self,
        request: DTerminalPipelineSnapshotRequest,
        link: LinkPipelineState,
    ) -> WTerminalPipelineSnapshot {
        self.log.push(format!(
            "pipeline.snapshot:{}:{}",
            request.request_id, link.queue_frames
        ));
        WTerminalPipelineSnapshot {
            request_id: request.request_id,
            dropped_targets: u32::try_from(link.queue_bytes).unwrap(),
            ..Default::default()
        }
    }
}

impl TerminalViewPort for Fakes {
    fn relay(&self, request: DTerminalViewRelay) {
        self.log.push(format!("view.relay:{}", request.socket_id));
    }
    fn close_socket(&self, socket_id: &str) {
        self.log.push(format!("view.close_socket:{socket_id}"));
    }
    fn drop_coordinator_sockets(&self) {
        self.log.push("view.drop_coordinator_sockets");
    }
}

impl LinkLifecyclePort for Fakes {
    fn on_open(&self) {
        self.log.push("lifecycle.on_open");
    }
    fn on_hello_ack(&self, terminal_metadata_negotiated: bool) {
        self.log.push(format!(
            "lifecycle.on_hello_ack:{terminal_metadata_negotiated}"
        ));
    }
    fn on_detach(&self) {
        self.log.push("lifecycle.on_detach");
    }
    fn on_writable(&self) {
        self.log.push("lifecycle.on_writable");
    }
    fn on_snapshot_ready(&self) {
        self.log.push("lifecycle.on_snapshot_ready");
    }
}

/// The link side of the dispatch: synchronous replies land in `replies`.
#[derive(Debug, Default)]
pub struct FakeLink {
    pub log: CallLog,
    pub replies: Vec<CoordWorkerUpstream>,
    pub commands: Vec<(Command, LinkFence)>,
    /// What the next event ack answers: whether it took the link live.
    pub goes_live: bool,
    pub state: LinkPipelineState,
}

impl DownstreamLink for FakeLink {
    fn reply(&mut self, frame: CoordWorkerUpstream) {
        self.replies.push(frame);
    }
    fn hello_acknowledged(&mut self, terminal_metadata_negotiated: bool) {
        self.log.push(format!(
            "link.hello_acknowledged:{terminal_metadata_negotiated}"
        ));
    }
    fn event_acknowledged(&mut self, client_seq: u64) -> bool {
        self.log
            .push(format!("link.event_acknowledged:{client_seq}"));
        self.goes_live
    }
    fn browser_command(&mut self, command: Command, fence: LinkFence) {
        self.commands.push((command, fence));
    }
    fn pipeline_state(&self) -> LinkPipelineState {
        self.state
    }
}

/// Let every spawned owner task that can run, run.
pub async fn settle_tasks() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

/// The next frame the uplink carries, or a failure after a bounded wait.
pub async fn next_uplink(receiver: &mut UplinkReceiver) -> CoordWorkerUpstream {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("an owner answered within the bound")
        .expect("the uplink channel is open")
}

impl LocalTerminalGrantPort for Fakes {
    fn install_grant(&self, request: &DLocalTerminalGrant) -> Result<(), String> {
        self.log
            .push(format!("local_terminal.install_grant:{}", request.grant_id));
        if request.grant_id.is_empty() {
            Err("grant_id is invalid".to_owned())
        } else {
            Ok(())
        }
    }

    fn revoke_device(&self, device_fingerprint: &str) {
        self.log
            .push(format!("local_terminal.revoke_device:{device_fingerprint}"));
    }
}

impl AttachmentLinkPort for Fakes {
    fn accept_relay_chunk(&self, chunk: DAttachmentChunk) -> OwnerFuture<RelayChunkOutcome> {
        self.log.push(format!(
            "attachments.accept_relay_chunk:{}",
            chunk.request_id
        ));
        self.settle(RelayChunkOutcome::Progress)
    }

    fn install_grant(&self, request: &DLocalAttachmentGrant) -> Result<(), String> {
        self.log
            .push(format!("attachments.install_grant:{}", request.grant_id));
        Ok(())
    }

    fn revoke_device(&self, device_fingerprint: &str) {
        self.log
            .push(format!("attachments.revoke_device:{device_fingerprint}"));
    }

    fn direct_status(&self, request: &DAttachmentDirectStatusRequest) -> AttachmentTransferStatus {
        self.log
            .push(format!("attachments.direct_status:{}", request.upload_id));
        AttachmentTransferStatus {
            upload_id: request.upload_id.clone(),
            ..Default::default()
        }
    }
}
