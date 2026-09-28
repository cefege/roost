//! Coordinator → worker frame dispatch: one explicit arm for every
//! `CoordWorkerDownstream` variant, each routed to its owner or answered with
//! what a v2 worker without that owner answers. Called by
//! `runtime::link_downstream::on_frame` for every decoded frame of the live link.
//! Ports v2 `apps/worker/src/transport/coord-link-downstream.ts`
//! (`handleDownstream`) and the dispatch half of `coord-link-direct-terminal.ts`.
//!
//! No frame here can be stale: `link_serve` owns exactly one socket per dial and
//! only frames read from it reach this file, which is v2's
//! `activeSocket() !== socket` guard made structural. Answers produced now go
//! through [`DownstreamLink::reply`] in receive order; answers an owner
//! produces later go through the [`Uplink`] fenced to the connection they
//! arrived on.

mod agent_prompt;
mod attachment_peer;
mod attachments;
mod direct;
mod keeper_update;
mod local_grants;
mod owner_task;
mod replies;
mod terminal;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};

use crate::browser_commands::Command;
use crate::link_ports::{DownstreamOwners, LinkPipelineState};
use crate::uplink::{LinkFence, Uplink};

/// What the dispatch needs from the link that read the frame.
pub trait DownstreamLink {
    /// Admit an answer now, in receive order, on the connection it answers.
    fn reply(&mut self, frame: CoordWorkerUpstream);
    /// Advance the replay barrier past the hello (v2 `outbox.acceptHelloAck`).
    fn hello_acknowledged(&mut self, terminal_metadata_negotiated: bool);
    /// Apply a durable or snapshot ack; true when THIS ack took the link live.
    fn event_acknowledged(&mut self, client_seq: u64) -> bool;
    /// Hand a browser command to the command pump, its answers fenced to `fence`.
    fn browser_command(&mut self, command: Command, fence: LinkFence);
    /// The link's own queue, for pipeline evidence (v2 `link.pipelineState()`).
    fn pipeline_state(&self) -> LinkPipelineState;
}

/// The dispatch, and the state it keeps across frames.
#[derive(Debug)]
pub struct Dispatcher {
    /// `None` is a worker without the terminal owners, which answers exactly
    /// as v2 does when those `CoordLinkDeps` callbacks are absent.
    owners: Option<DownstreamOwners>,
    uplink: Uplink,
    /// This worker process's epoch, stamped into the refusals v2 stamps it in.
    process_epoch: String,
    stream_requests_in_flight: Arc<AtomicUsize>,
}

impl Dispatcher {
    pub fn new(
        uplink: Uplink,
        process_epoch: impl Into<String>,
        owners: Option<DownstreamOwners>,
    ) -> Self {
        Self {
            owners,
            uplink,
            process_epoch: process_epoch.into(),
            stream_requests_in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn owners(&self) -> Option<&DownstreamOwners> {
        self.owners.as_ref()
    }

    /// Route one frame. `received` is the origin every request budget counts
    /// from.
    pub fn dispatch(
        &self,
        frame: CoordWorkerDownstream,
        received: Instant,
        link: &mut dyn DownstreamLink,
    ) {
        let kind = frame.kind();
        tracing::trace!(kind, "dispatching a downstream frame");
        match frame {
            CoordWorkerDownstream::HelloAck { capabilities, .. } => {
                self.hello_ack(&capabilities, link);
            }
            CoordWorkerDownstream::Ping { ts, .. } => {
                link.reply(CoordWorkerUpstream::Pong { ts, trace_id: None });
            }
            CoordWorkerDownstream::BrowserCommand {
                browser_id,
                viewer_id,
                request_id,
                frame,
                ..
            } => {
                let command = Command::new(browser_id, viewer_id, request_id, frame);
                link.browser_command(command, self.uplink.fence());
            }
            CoordWorkerDownstream::EventAck(ack) => self.event_ack(ack.client_seq, link),
            CoordWorkerDownstream::Binary(binary) => self.binary(binary),
            CoordWorkerDownstream::InputRequest(request) => {
                self.input_request(request, received, link);
            }
            CoordWorkerDownstream::TerminalStreamState(request) => {
                self.stream_state(request, received, link);
            }
            CoordWorkerDownstream::TerminalSnapshotRequest(request) => {
                self.snapshot_request(request);
            }
            CoordWorkerDownstream::TerminalPipelineSnapshot(request) => {
                self.pipeline_snapshot(request, link);
            }
            CoordWorkerDownstream::TerminalViewRelay(request) => self.view_relay(request),
            CoordWorkerDownstream::TerminalViewSocketClosed(request) => {
                self.view_socket_closed(&request.socket_id);
            }
            CoordWorkerDownstream::TerminalInputRouteClaim(request) => {
                self.route_claim(request, received, link);
            }
            CoordWorkerDownstream::AgentPrompt(request) => {
                self.agent_prompt(request, received, link);
            }
            CoordWorkerDownstream::LocalTerminalGrant(request) => {
                self.local_terminal_grant(request, link);
            }
            CoordWorkerDownstream::LocalAttachmentGrant(request) => {
                self.attachment_grant(request, link);
            }
            CoordWorkerDownstream::KeeperUpdatePrepare(request) => {
                self.keeper_update_prepare(request, link);
            }
            CoordWorkerDownstream::LocalTerminalPeerOffer(request) => {
                self.terminal_peer_offer(request, received, link);
            }
            CoordWorkerDownstream::LocalAttachmentPeerOffer(request) => {
                self.attachment_peer_offer(request, received, link);
            }
            CoordWorkerDownstream::AttachmentDirectStatusRequest(request) => {
                self.attachment_status(request, link);
            }
            CoordWorkerDownstream::UpdateBroker(request) => {
                link.reply(replies::update_broker_refusal(request));
            }
            // v2's absent-owner behaviour for these is `deps.onX?.()`: nothing
            // is sent (coord-link-direct-terminal.ts:155-157,193-195;
            // coord-link-downstream.ts:297-300,324-331).
            CoordWorkerDownstream::TerminalTransportProbe(request) => {
                self.terminal_transport_probe(&request, link);
            }
            CoordWorkerDownstream::LocalTerminalGrantRevoke(request) => {
                self.local_terminal_revoke(&request);
            }
            CoordWorkerDownstream::LocalAttachmentGrantRevoke(request) => {
                self.attachment_grant_revoke(&request);
            }
            CoordWorkerDownstream::LocalTerminalPeerCancel(request) => {
                self.terminal_peer_cancel(&request);
            }
            CoordWorkerDownstream::LocalAttachmentPeerCancel(request) => {
                self.attachment_peer_cancel(&request);
            }
            CoordWorkerDownstream::TerminalDirectRetire(request) => {
                self.terminal_direct_retire(&request);
            }
            CoordWorkerDownstream::AttachmentChunk(chunk) => self.attachment_chunk(chunk),
            // v2 has no case for these: a retired schema tag stays inert.
            CoordWorkerDownstream::CoordMovePrepare(_) => retired(kind),
            CoordWorkerDownstream::CoordMoveSnapshotStart(_) => retired(kind),
            CoordWorkerDownstream::CoordMoveSnapshotChunk(_) => retired(kind),
            CoordWorkerDownstream::CoordRelocate(_) => retired(kind),
        }
    }
}

fn retired(kind: &'static str) {
    tracing::debug!(kind, "a retired downstream schema tag was ignored");
}
