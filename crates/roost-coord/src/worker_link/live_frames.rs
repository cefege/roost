//! The two SYNCHRONOUS arms: terminal and semantic live frames, and the replies
//! that settle a request the coordinator is holding open.
//!
//! Ported from the `handleLiveFrame` half of
//! `apps/coord/src/workers/worker-frame-dispatch.ts:248-395`. Split from
//! `frame_dispatch` on the same seam that file's own header draws: only the
//! durable arm awaits the database, so only it is asynchronous, and putting the
//! two synchronous arms in a sibling `impl` keeps the boxed-future machinery on
//! one side of the line where it is worth paying for.
//!
//! The agent-status arm is v2's `agents/worker-agent-status-frame.ts` as well:
//! it builds the update from the upstream frame and hands it to the hub, and
//! the hub owns every question about whether the report may be applied.
//!
//! NEITHER ARM OWNS A SOCKET. Both resolve to synchronous coordinator state —
//! the byte hub, the view hub, a bus, the pending-RPC table, or the agent tunnel
//! registry — and neither can ask for a close it does not understand. An arm
//! with no destination is `Refused`, which is the read loop's signal that this
//! dispatcher did not handle the frame; it is not a claim that it was malformed.

use roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1;
use roost_protocol::wire::agent_status::AgentStatusUpdate;
use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, TerminalMetadata};
use roost_protocol::wire::{ChannelId, WorkerFp};

use crate::terminal_screen::pipeline_snapshot::is_terminal_pipeline_snapshot_wire_shape;
use crate::terminal_screen::typed_results::TypedWorkerResult;
use crate::worker_link::dispatch::{DispatchOutcome, InboundFrame};
use crate::worker_link::frame_dispatch::WorkerFrameDispatcher;

impl WorkerFrameDispatcher {
    /// One live frame: bytes, cells, or a semantic observation.
    pub(crate) fn handle_live(&self, worker_fp: &str, frame: InboundFrame) -> DispatchOutcome {
        let InboundFrame {
            channel,
            frame: upstream,
            ..
        } = frame;
        if self.fenced("live") {
            return DispatchOutcome::Refused;
        }
        let Ok(worker) = self.authenticated(worker_fp) else {
            return self.refuse(channel, "unaddressable_worker_fp");
        };
        match upstream {
            CoordWorkerUpstream::CellGrid(mut grid) => {
                let Some(declared) = self.declared_channel(channel, grid.channel_id) else {
                    return DispatchOutcome::Refused;
                };
                let Some(body) = grid.frame.as_option_mut() else {
                    return self.refuse(channel, "cell_grid_carried_no_frame");
                };
                self.core.services.byte_hub.publish_cell_grid(
                    &worker,
                    declared,
                    body,
                    crate::serve::now_ms(),
                );
                DispatchOutcome::Handled
            }
            CoordWorkerUpstream::CellGridChunk(mut chunk) => {
                let Some(declared) = self.declared_channel(channel, chunk.channel_id) else {
                    return DispatchOutcome::Refused;
                };
                let Some(body) = chunk.chunk.as_option_mut() else {
                    return self.refuse(channel, "cell_grid_chunk_carried_no_chunk");
                };
                self.core.services.byte_hub.publish_cell_grid_chunk(
                    &worker,
                    declared,
                    body,
                    crate::serve::now_ms(),
                );
                DispatchOutcome::Handled
            }
            CoordWorkerUpstream::TerminalMetadata(metadata) => {
                self.publish_metadata(&worker, &metadata)
            }
            CoordWorkerUpstream::Binary(_) => {
                // v2 parses a legacy worker's raw output for a title. The
                // capability cutover makes that path dead — v2 refuses it too once
                // `terminal_metadata_v1` is negotiated — and this build has never
                // carried the parser, so the refusal is the whole behaviour and
                // claiming otherwise would be a lie about a frame we drop.
                self.refuse(channel, "legacy_binary_metadata_is_not_parsed")
            }
            CoordWorkerUpstream::TerminalViewState(state) => {
                self.core.services.views.apply_owner_view_state(
                    &worker,
                    state.socket_id.as_str(),
                    &state.frame,
                );
                DispatchOutcome::Handled
            }
            CoordWorkerUpstream::TerminalViewProjection(projection) => {
                self.core
                    .services
                    .views
                    .apply_owner_projection(&worker, &projection);
                DispatchOutcome::Handled
            }
            CoordWorkerUpstream::AgentStatus(frame_status) => {
                let update = AgentStatusUpdate {
                    common: frame_status.status.common,
                    active: frame_status.status.active,
                };
                let Ok(value) = serde_json::to_value(&update) else {
                    return self.refuse(channel, "agent_status_has_no_wire_form");
                };
                self.core
                    .services
                    .agents
                    .status
                    .accept_worker_status(&self.core, &worker, value);
                DispatchOutcome::Handled
            }
            _ => self.refuse(channel, "live_arm_has_no_destination"),
        }
    }

    /// One reply to a request the coordinator is holding open.
    ///
    /// Every settling arm is the same table and the same `request_id`; v2
    /// routes a typed result, a stream result and a pipeline sample through the
    /// identical `resolvePendingRpc` an `rpc-ok` uses. The table key carries the
    /// authenticated fingerprint, so another worker's reply settles nothing.
    /// Before the snapshot barrier nothing settles (v2 `pendingResultWorker`).
    pub(crate) fn handle_rpc(&self, worker_fp: &str, frame: InboundFrame) -> DispatchOutcome {
        let InboundFrame {
            channel,
            frame: upstream,
            ..
        } = frame;
        if self.fenced("rpc") {
            return DispatchOutcome::Refused;
        }
        let Ok(worker) = self.authenticated(worker_fp) else {
            return self.refuse(channel, "unaddressable_worker_fp");
        };
        // Progress settles no pending RPC, so the snapshot barrier does not
        // gate it: v2 routes it on any socket past hello.
        if let CoordWorkerUpstream::UpdateProgress(progress) = &upstream {
            return match self
                .core
                .services
                .deploy
                .accept_update_progress(&worker, progress)
            {
                Ok(()) => DispatchOutcome::Handled,
                Err(reason) => self.refuse(channel, reason),
            };
        }
        if !self.handle.is_ready() {
            tracing::debug!(worker_fp = %worker, what = upstream.kind(),
                "worker link: unready_rpc_result; dropped");
            return DispatchOutcome::Refused;
        }
        let pending = self.core.services.scrollback.pending();
        if matches!(
            upstream,
            CoordWorkerUpstream::AgentToolOutput(_) | CoordWorkerUpstream::AgentToolResult(_)
        ) {
            return if self.core.services.agent_tools.receive(worker_fp, upstream) {
                DispatchOutcome::Handled
            } else {
                self.refuse(channel, "agent_tool_frame_has_no_pending_call")
            };
        }
        match upstream {
            CoordWorkerUpstream::RpcOk {
                request_id, data, ..
            } => {
                pending.resolve(&request_id, data, Some(worker.as_str()));
            }
            CoordWorkerUpstream::RpcError {
                request_id,
                message,
                ..
            } => {
                pending.reject(&request_id, &message, Some(worker.as_str()));
            }
            CoordWorkerUpstream::InputResult(result) => {
                pending.resolve_typed(TypedWorkerResult::Input(result), Some(worker.as_str()));
            }
            direct @ (CoordWorkerUpstream::TerminalInputRouteResult(_)
            | CoordWorkerUpstream::TerminalTransportProbeResult(_)
            | CoordWorkerUpstream::LocalTerminalPeerAnswer(_)
            | CoordWorkerUpstream::LocalTerminalPeerError(_)
            | CoordWorkerUpstream::LocalAttachmentPeerAnswer(_)
            | CoordWorkerUpstream::LocalAttachmentPeerError(_)
            | CoordWorkerUpstream::AttachmentDirectStatus(_)) => {
                return crate::worker_link::direct_results::accept_direct_result(
                    &self.core.services,
                    &self.handle,
                    direct,
                );
            }
            CoordWorkerUpstream::TerminalPipelineSnapshot(snapshot) => {
                if !is_terminal_pipeline_snapshot_wire_shape(&snapshot) {
                    return self.refuse(channel, "invalid_terminal_pipeline_snapshot");
                }
                pending.resolve_typed(
                    TypedWorkerResult::PipelineSnapshot(snapshot),
                    Some(worker.as_str()),
                );
            }
            _ => return self.refuse(channel, "rpc_arm_has_no_destination"),
        }
        DispatchOutcome::Handled
    }

    /// The fingerprint a frame may act on: the transport's, branded.
    fn authenticated(&self, worker_fp: &str) -> Result<WorkerFp, ()> {
        WorkerFp::try_from(worker_fp.to_owned()).map_err(|_| ())
    }

    /// The channel a frame addresses, refused when the header and the payload
    /// disagree.
    ///
    /// `InboundFrame::channel` is the read loop's classification and the payload
    /// carries the same value a second time. Two answers to "which channel" on
    /// one frame means the read loop and the body disagree, and delivering cells
    /// to whichever won would bind a PTY to a session nobody announced.
    fn declared_channel(&self, channel: u32, carried: u32) -> Option<ChannelId> {
        if channel != carried {
            tracing::warn!(
                worker_fp = %self.handle.worker_fp,
                header_channel = channel,
                carried_channel = carried,
                "a live frame's header and body named different channels"
            );
            return None;
        }
        ChannelId::try_from(i64::from(carried)).ok()
    }

    /// One semantic metadata record: a title, an activity stamp, or both.
    ///
    /// v2's `acceptTerminalMetadata`. The session is resolved through the byte
    /// hub rather than trusted from the frame, and an unmapped channel is a drop
    /// with a reason rather than a title attributed to a session that may not
    /// own it.
    fn publish_metadata(&self, worker: &WorkerFp, metadata: &TerminalMetadata) -> DispatchOutcome {
        if !self
            .handle
            .capabilities
            .contains(CAPABILITY_TERMINAL_METADATA_V1)
        {
            tracing::debug!(
                worker_fp = %worker,
                "a semantic metadata frame arrived without the negotiated capability"
            );
            return DispatchOutcome::Refused;
        }
        let services = &self.core.services;
        let Some(session_id) = services.byte_hub.resolve(worker, metadata.channel_id) else {
            tracing::debug!(
                worker_fp = %worker,
                channel_id = metadata.channel_id.as_u32(),
                "a metadata frame arrived on an unbound channel"
            );
            return DispatchOutcome::Refused;
        };
        if metadata.title_changed {
            services
                .titles
                .observe_title(&services.buses, session_id.as_str(), &metadata.title);
        }
        if metadata.clipboard_changed {
            services.buses.clipboard_bus.publish(
                crate::events::bus_messages::SessionClipboardWrite {
                    session_id: session_id.as_str().to_owned(),
                    text: metadata.clipboard.clone(),
                },
            );
            // The length only: the text is the operator's clipboard.
            tracing::debug!(
                worker_fp = %worker,
                clipboard_bytes = metadata.clipboard.len(),
                "terminal emitted an OSC 52 clipboard write"
            );
            let core = self.core.clone();
            let source_session_id = session_id.as_str().to_owned();
            let source_worker_fp = worker.as_str().to_owned();
            let text = metadata.clipboard.clone();
            tokio::spawn(async move {
                if let Err(error) = crate::clipboard::capture_osc52(
                    &core,
                    &source_session_id,
                    &source_worker_fp,
                    &text,
                    crate::serve::now_ms(),
                )
                .await
                {
                    tracing::error!(error = %error, clipboard_bytes = text.len(), "OSC 52 history persistence failed");
                }
            });
        }
        if metadata.command_finished {
            services.buses.command_finished_bus.publish(
                crate::events::bus_messages::SessionCommandFinished {
                    session_id: session_id.as_str().to_owned(),
                    exit_code: metadata.command_exit_code,
                    duration_ms: metadata.command_duration_ms,
                },
            );
            tracing::debug!(
                worker_fp = %worker,
                exit_code = ?metadata.command_exit_code,
                duration_ms = metadata.command_duration_ms,
                "a long shell command finished"
            );
        }
        if metadata.bell {
            services
                .buses
                .bell_bus
                .publish(crate::events::bus_messages::SessionBell {
                    session_id: session_id.as_str().to_owned(),
                });
            tracing::debug!(worker_fp = %worker, session_id = %session_id, "terminal bell received");
        }
        if metadata.progress.is_some()
            || metadata.user_vars_changed
            || !metadata.notifications.is_empty()
        {
            // A coalesced record can carry several notifications; each
            // reaches the browser, the retained facts ride the first.
            let mut progress = metadata.progress;
            let mut user_vars = metadata
                .user_vars_changed
                .then(|| metadata.user_vars.clone());
            let mut notifications = metadata
                .notifications
                .iter()
                .cloned()
                .map(Some)
                .collect::<Vec<_>>();
            if notifications.is_empty() {
                notifications.push(None);
            }
            for notification in notifications {
                services.terminal_signals.observe(
                    &services.buses,
                    session_id.as_str(),
                    progress.take(),
                    user_vars.take(),
                    notification,
                );
            }
        }
        if metadata.activity_changed
            && let Ok(observed_at_ms) = i64::try_from(metadata.activity_ts_ms)
        {
            services.feed.last_activity().observe_and_publish(
                &services.buses,
                session_id.as_str(),
                observed_at_ms,
            );
        }
        DispatchOutcome::Handled
    }
}
