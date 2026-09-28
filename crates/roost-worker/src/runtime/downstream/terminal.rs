//! The downstream arms whose owners exist: link lifecycle (hello-ack, the
//! snapshot-ready edge), browser input, coordinator stream state and repair,
//! pipeline evidence, worker-owned views, and input-route claims. Called by
//! [`super::Dispatcher::dispatch`]. Ports those cases of v2
//! `apps/worker/src/transport/coord-link-downstream.ts` and
//! `coord-link-direct-terminal.ts`, with the ordering of `coord-link-deps.ts`.

use std::sync::Arc;
use std::time::Instant;

use roost_proto::{
    DInputRequest, DTerminalInputRouteClaim, DTerminalPipelineSnapshotRequest,
    DTerminalStreamState, DTerminalViewRelay,
};
use roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1;
use roost_protocol::wire::coord_worker::{
    Binary, CoordWorkerUpstream, DIR_TO_PTY, TerminalInputStatus, TerminalSnapshotRequest,
    TerminalStreamFailureKind, TerminalStreamStatus, TerminalWritePhase,
};

use super::owner_task::{InFlightSlot, run_owner};
use super::replies::{self, StreamResultKey};
use super::{Dispatcher, DownstreamLink};
use crate::uplink::terminal_results::InputResultKey;
use crate::uplink::{RequestBudget, TERMINAL_STREAM_REQUEST_INFLIGHT_CAP};

impl Dispatcher {
    /// v2 order: the barrier first (`acceptHelloAck`), then the coordinator's
    /// browser sockets are dropped, then the session half is told.
    pub(super) fn hello_ack(&self, capabilities: &[String], link: &mut dyn DownstreamLink) {
        let negotiated = capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_TERMINAL_METADATA_V1);
        link.hello_acknowledged(negotiated);
        if let Some(owners) = &self.owners {
            owners.view.drop_coordinator_sockets();
            owners.lifecycle.on_hello_ack(negotiated);
        }
        tracing::info!(
            terminal_metadata_v1 = negotiated,
            "the coordinator acknowledged the hello"
        );
    }

    /// One sequence space serves both durable events and the snapshot; the ack
    /// that takes the barrier live is v2's `onLive` → `onSnapshotReady`.
    pub(super) fn event_ack(&self, client_seq: u64, link: &mut dyn DownstreamLink) {
        if !link.event_acknowledged(client_seq) {
            return;
        }
        tracing::info!(client_seq, "the snapshot is acknowledged; the link is live");
        if let Some(owners) = &self.owners {
            owners.lifecycle.on_snapshot_ready();
        }
    }

    /// v2 `onBinary`: only `DIR_TO_PTY` is input.
    pub(super) fn binary(&self, binary: Binary) {
        if binary.direction != DIR_TO_PTY {
            tracing::debug!(channel = %binary.channel_id, direction = binary.direction, "a downstream binary frame was not addressed to a PTY");
            return;
        }
        match &self.owners {
            Some(owners) => owners.input.write_binary(binary.channel_id, binary.data),
            None => {
                tracing::warn!(channel = %binary.channel_id, "downstream input arrived with no input owner attached")
            }
        }
    }

    pub(super) fn input_request(
        &self,
        request: DInputRequest,
        received: Instant,
        link: &mut dyn DownstreamLink,
    ) {
        let key = InputResultKey::from(&request);
        let Some(owners) = &self.owners else {
            let refusal = replies::input_result(
                &key,
                TerminalInputStatus::Rejected,
                replies::INPUT_HANDLER_UNAVAILABLE,
            );
            if let Some(frame) = refusal {
                link.reply(frame);
            }
            return;
        };
        let budget = RequestBudget::from_budget_ms(request.budget_ms, received);
        let fence = self.uplink.fence();
        let reply_fence = fence.clone();
        let uplink = self.uplink.clone();
        let input = Arc::clone(&owners.input);
        run_owner(
            move || input.write_input(request, budget, fence),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(Some(result)) => Some(CoordWorkerUpstream::InputResult(result)),
                    Ok(None) => {
                        tracing::warn!(request_id = %key.request_id, "the input owner has no result the wire can carry");
                        None
                    }
                    Err(message) => {
                        tracing::warn!(request_id = %key.request_id, error = %message, "the input owner failed");
                        replies::input_result(&key, TerminalInputStatus::Ambiguous, &message)
                    }
                };
                if let Some(frame) = frame {
                    uplink.send_fenced(&reply_fence, frame);
                }
            }),
        );
    }

    /// Bounded by its own admission; the slot is held until the reply is sent.
    pub(super) fn stream_state(
        &self,
        request: DTerminalStreamState,
        received: Instant,
        link: &mut dyn DownstreamLink,
    ) {
        let key = StreamResultKey::from(&request);
        let Some(slot) = InFlightSlot::try_take(
            &self.stream_requests_in_flight,
            TERMINAL_STREAM_REQUEST_INFLIGHT_CAP,
        ) else {
            tracing::warn!(
                request_id = %key.request_id(),
                in_flight = TERMINAL_STREAM_REQUEST_INFLIGHT_CAP,
                "terminal-stream admission is full; the request is refused before any write"
            );
            let refusal = key.untouched(
                TerminalStreamStatus::Rejected,
                TerminalWritePhase::PreWrite,
                TerminalStreamFailureKind::RetryablePreWrite,
                replies::STREAM_ADMISSION_FULL,
            );
            if let Some(frame) = refusal {
                link.reply(frame);
            }
            return;
        };
        let Some(owners) = &self.owners else {
            tracing::warn!(request_id = %key.request_id(), "a terminal-stream request arrived with no stream owner attached");
            return;
        };
        let budget = RequestBudget::from_budget_ms(request.budget_ms, received);
        let fence = self.uplink.fence();
        let reply_fence = fence.clone();
        let uplink = self.uplink.clone();
        let stream = Arc::clone(&owners.stream);
        run_owner(
            move || stream.apply_stream_state(request, budget, fence),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(Some(result)) => Some(CoordWorkerUpstream::TerminalStreamResult(result)),
                    Ok(None) => {
                        tracing::warn!(request_id = %key.request_id(), "the terminal-stream owner has no result the wire can carry");
                        None
                    }
                    Err(message) => {
                        tracing::warn!(request_id = %key.request_id(), error = %message, "the terminal-stream owner failed");
                        key.untouched(
                            TerminalStreamStatus::Ambiguous,
                            TerminalWritePhase::Unknown,
                            TerminalStreamFailureKind::AmbiguousBoundary,
                            &message,
                        )
                    }
                };
                if let Some(frame) = frame {
                    uplink.send_fenced(&reply_fence, frame);
                }
                drop(slot);
            }),
        );
    }

    pub(super) fn snapshot_request(&self, request: TerminalSnapshotRequest) {
        match &self.owners {
            Some(owners) => owners.stream.request_snapshot(request),
            None => {
                tracing::warn!(session = %request.session_id, "a snapshot request arrived with no stream owner attached")
            }
        }
    }

    /// Answered synchronously, on the connection it arrived on.
    pub(super) fn pipeline_snapshot(
        &self,
        request: DTerminalPipelineSnapshotRequest,
        link: &mut dyn DownstreamLink,
    ) {
        let Some(owners) = &self.owners else {
            tracing::warn!(request_id = %request.request_id, "a pipeline snapshot request arrived with no pipeline owner attached");
            return;
        };
        let snapshot = owners
            .pipeline
            .pipeline_snapshot(request, link.pipeline_state());
        link.reply(CoordWorkerUpstream::TerminalPipelineSnapshot(snapshot));
    }

    /// Synchronous, so one browser socket's decisions keep their receive order.
    pub(super) fn view_relay(&self, request: DTerminalViewRelay) {
        match &self.owners {
            Some(owners) => owners.view.relay(request),
            None => {
                tracing::warn!(socket = %request.socket_id, "a view relay arrived with no view owner attached")
            }
        }
    }

    /// v2 order: the socket's input route is retired before its views close.
    pub(super) fn view_socket_closed(&self, socket_id: &str) {
        match &self.owners {
            Some(owners) => {
                owners.input.retire_connection(socket_id);
                owners.view.close_socket(socket_id);
            }
            None => tracing::warn!(
                socket = socket_id,
                "a view socket closed with no view owner attached"
            ),
        }
    }

    /// Every answer is fenced; no claim, a failed claim, and no owner are all
    /// v2's `refusedClaim`.
    pub(super) fn route_claim(
        &self,
        request: DTerminalInputRouteClaim,
        received: Instant,
        link: &mut dyn DownstreamLink,
    ) {
        let refused = replies::refused_claim(&request, &self.process_epoch);
        let Some(owners) = &self.owners else {
            link.reply(refused);
            return;
        };
        let request_id = request.request_id.clone();
        let budget = RequestBudget::from_budget_ms(request.budget_ms, received);
        let fence = self.uplink.fence();
        let reply_fence = fence.clone();
        let uplink = self.uplink.clone();
        let input = Arc::clone(&owners.input);
        run_owner(
            move || input.claim_route(request, budget, fence),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(Some(result)) => replies::route_result(request_id, result),
                    Ok(None) => refused,
                    Err(message) => {
                        tracing::warn!(request_id, error = %message, "the input route owner failed");
                        refused
                    }
                };
                uplink.send_fenced(&reply_fence, frame);
            }),
        );
    }
}
