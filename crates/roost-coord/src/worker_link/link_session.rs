//! One worker link after its hello: the generation it claimed, its heartbeat,
//! its durable-event window, the owed reaps it carries, and the teardown that
//! gives all of it back.
//!
//! Called only by `worker_link::connection`'s read loop. Ports the post-hello
//! half of `apps/coord/src/workers/worker-conn.ts` (hello admission, pong,
//! supersede, revoke, close, the delayed respawn) and the per-message admission
//! of `worker-ws-handler.ts` (superseded fence, readiness gate, rate window);
//! the completion lane beside a durable append is `worker_link::result_lane`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::extract::ws::WebSocket;
use roost_protocol::versioning::{
    CAPABILITY_TERMINAL_METADATA_V1, CAPABILITY_TERMINAL_VIEW_OWNER_V1,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use roost_protocol::wire::{SessionId, WorkerFp};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;

use crate::coord_core::seams::WorkerRouteIndex as _;
use crate::coord_core::worker_handle::WorkerHandle;
use crate::coord_core::worker_lifecycle::LinkEnd;
use crate::services::CoordServices;
use crate::terminal_view::OwnerRegistration;
use crate::worker_link::conn_types::SocketClose;
use crate::worker_link::dispatch::{DispatchOutcome, FrameClass, FrameDispatch, InboundFrame};
use crate::worker_link::frame_dispatch::WorkerFrameDispatcher;
use crate::worker_link::handshake::{acknowledged_capabilities, credential_refresh_accepted};
use crate::worker_link::keepalive::{KeepaliveDue, PingSchedule};
use crate::worker_link::rate_window::DurableEventWindow;
use crate::worker_link::reap_outbox::ReapOutbox;
use crate::worker_link::result_lane::ResultLane;
use crate::worker_link::upstream_frame::{HelloFrame, LinkFrame, decode_link_frame};
use crate::workers::registry::{claim_generation, publish_routable};
use crate::workers::respawn::respawn_missing_for_worker;
use crate::workers::send::reap_orphan_pty;

/// How long after readiness the respawn-if-missing pass runs (v2
/// `scheduleRespawn`, `worker-conn.ts:151-160`): long enough for the worker's
/// own post-snapshot replay to land first.
pub(super) const RESPAWN_DELAY: Duration = Duration::from_secs(3);

/// What the read loop does after one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkStep {
    /// Keep reading.
    Continue,
    /// End the socket with this close.
    Close(SocketClose),
}

/// The two ways the link's owner reaches this socket from another task.
pub(super) struct LinkTransport {
    /// Where the handle's `send` enqueues; the read loop writes what arrives.
    pub(super) outbound: mpsc::UnboundedSender<CoordWorkerDownstream>,
    /// Notified when a newer hello supersedes this generation.
    pub(super) close_requested: Arc<Notify>,
}

/// One admitted link, past its hello.
pub(super) struct LinkSession {
    services: Arc<CoordServices>,
    handle: Arc<WorkerHandle>,
    dispatcher: WorkerFrameDispatcher,
    result_lane: ResultLane,
    keepalive: PingSchedule,
    event_window: DurableEventWindow,
    reaps: ReapOutbox,
    view_owner: Option<OwnerRegistration>,
    respawn_at: Option<Instant>,
    announced_ready: bool,
}

impl LinkSession {
    /// Admit a hello: claim the fingerprint, supersede the prior generation,
    /// and acknowledge exactly the capabilities this coordinator serves.
    /// `None` only when no generation identity could be minted.
    pub(super) fn claim(
        services: &Arc<CoordServices>,
        hello: HelloFrame,
        transport: LinkTransport,
    ) -> Option<Self> {
        let worker_fp = hello.worker_fp;
        let generation = match crate::coord_core::ids::draw::<16>() {
            Ok(bytes) => crate::coord_core::ids::render_v4(bytes),
            Err(error) => {
                tracing::error!(%worker_fp, %error, "worker link: no entropy for a generation id");
                return None;
            }
        };
        let acknowledged =
            acknowledged_capabilities(&hello.capabilities, &services.worker_lifecycle);
        let handle = Arc::new(
            WorkerHandle::new(
                worker_fp.clone(),
                hello.process_epoch,
                generation,
                acknowledged.iter().cloned().collect(),
                enqueue_into(transport.outbound),
            )
            .with_close(Arc::new(move || transport.close_requested.notify_one())),
        );

        let superseded = services.workers.current(&worker_fp);
        if let Some(prior) = &superseded {
            services.worker_lifecycle.superseded(prior);
        }
        claim_generation(&services.buses, &services.workers, Arc::clone(&handle));
        if let Some(prior) = superseded {
            tracing::info!(%worker_fp, prior = %prior.connection_generation,
                "worker link: superseded_prior_connection");
            prior.request_close();
        }
        // An unready hello leaves every public route; the exact snapshot
        // installs the live index after its durable commit.
        services
            .byte_hub
            .replace_worker_channel_index(&worker_fp, &[]);
        let owns_views = handle
            .capabilities
            .contains(CAPABILITY_TERMINAL_VIEW_OWNER_V1);
        let view_owner = if owns_views {
            Some(services.views.register_owner(&worker_fp))
        } else {
            services.views.clear_owner(&worker_fp);
            None
        };
        let mut keepalive = PingSchedule::new();
        keepalive.schedule_next_ping(Instant::now());
        if handle.send(CoordWorkerDownstream::HelloAck {
            capabilities: acknowledged,
            trace_id: None,
        }) == 0
        {
            tracing::warn!(%worker_fp, "worker link: send_failed for hello_ack");
        }
        services.worker_lifecycle.hello_acknowledged(&handle);
        tracing::info!(
            %worker_fp,
            connection_generation = %handle.connection_generation,
            terminal_metadata_v1 = handle.capabilities.contains(CAPABILITY_TERMINAL_METADATA_V1),
            terminal_view_owner_v1 = owns_views,
            acknowledged = handle.capabilities.len(),
            "worker link: hello"
        );
        Some(Self {
            dispatcher: services.worker_dispatcher(Arc::clone(&handle)),
            result_lane: ResultLane::new(services.worker_dispatcher(Arc::clone(&handle))),
            reaps: ReapOutbox::attach(&services.orphan_kills, worker_fp),
            services: Arc::clone(services),
            handle,
            keepalive,
            event_window: DurableEventWindow::new(),
            view_owner,
            respawn_at: None,
            announced_ready: false,
        })
    }

    /// When the read loop must wake for a timer, if at all.
    pub(super) fn next_wake(&self) -> Option<Instant> {
        match (self.keepalive.next_wake(), self.respawn_at) {
            (Some(ping), Some(respawn)) => Some(ping.min(respawn)),
            (ping, respawn) => ping.or(respawn),
        }
    }

    /// A timer fired: the heartbeat, the delayed respawn, and owed reaps.
    pub(super) fn on_wake(&mut self, now: Instant) -> LinkStep {
        if let Some(close) = self.fenced_close() {
            return LinkStep::Close(close);
        }
        match self.keepalive.fire(now) {
            KeepaliveDue::Ping { generation } => {
                let ping = CoordWorkerDownstream::Ping {
                    ts: i64::try_from(generation).unwrap_or(i64::MAX),
                    trace_id: None,
                };
                if self.handle.send(ping) == 0 {
                    tracing::warn!(worker_fp = %self.handle.worker_fp, "worker link: send_failed for keepalive");
                }
            }
            KeepaliveDue::PongTimeout { generation } => {
                tracing::warn!(worker_fp = %self.handle.worker_fp, ping_generation = generation,
                    "worker link: pong_timeout; closing");
                return LinkStep::Close(SocketClose::Default);
            }
            KeepaliveDue::Nothing => {}
        }
        if self.respawn_at.is_some_and(|at| now >= at) {
            self.respawn_at = None;
            self.spawn_respawn();
        }
        self.deliver_owed_reaps();
        LinkStep::Continue
    }

    /// One data message's bytes; a durable append reads `socket` while it runs.
    pub(super) async fn on_bytes(&mut self, bytes: &[u8], socket: &mut WebSocket) -> LinkStep {
        if let Some(close) = self.fenced_close() {
            return LinkStep::Close(close);
        }
        let worker_fp = &self.handle.worker_fp;
        let frame = match decode_link_frame(bytes) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%worker_fp, %error, "worker link: decode_failed; the frame is ignored");
                return LinkStep::Continue;
            }
        };
        if !self.handle.is_ready() && !frame.crosses_snapshot_barrier() {
            tracing::debug!(%worker_fp, frame = frame.kind(),
                "worker link: frame_before_snapshot_ready; dropped");
            return LinkStep::Continue;
        }
        match frame {
            LinkFrame::Hello(_) => {
                tracing::warn!(%worker_fp, "worker link: duplicate_hello; closing");
                LinkStep::Close(SocketClose::Default)
            }
            LinkFrame::Pong { ts } => {
                if !self.keepalive.accept_pong(ts, Instant::now()) {
                    tracing::debug!(%worker_fp, ts, "worker link: a pong for no outstanding ping");
                }
                LinkStep::Continue
            }
            LinkFrame::RefreshJwt { jwt } => {
                let accepted =
                    credential_refresh_accepted(&self.services, worker_fp.as_str(), &jwt).await;
                if accepted {
                    LinkStep::Continue
                } else {
                    LinkStep::Close(SocketClose::Default)
                }
            }
            LinkFrame::Dispatch(frame) => self.dispatch(*frame, socket).await,
        }
    }

    /// Hand one frame to the dispatcher, then act on what it changed.
    async fn dispatch(&mut self, frame: InboundFrame, socket: &mut WebSocket) -> LinkStep {
        let worker_fp = self.handle.worker_fp.clone();
        let outcome = if frame.class == FrameClass::Durable {
            let now_ms = u64::try_from(crate::serve::now_ms()).unwrap_or(0);
            if let Err(breach) = self.event_window.admit(now_ms) {
                tracing::warn!(%worker_fp, limit = breach.limit, window_ms = breach.window_ms,
                    "worker link: event_rate_exceeded; closing");
                return LinkStep::Close(SocketClose::EventRateExceeded);
            }
            let append = self.dispatcher.handle_durable(worker_fp.as_str(), frame);
            self.result_lane.await_append(append, socket).await
        } else {
            self.dispatcher.handle_now(worker_fp.as_str(), frame)
        };
        if let DispatchOutcome::Close(close) = outcome {
            tracing::info!(%worker_fp, reason = close.reason(), "worker link: closing on policy");
            return LinkStep::Close(close);
        }
        if let Some(close) = self.result_lane.take_close() {
            return LinkStep::Close(close);
        }
        if !self.announced_ready && self.handle.is_ready() {
            self.announced_ready = true;
            self.respawn_at = Some(Instant::now() + RESPAWN_DELAY);
            self.services.worker_lifecycle.ready(&self.handle);
        }
        self.deliver_owed_reaps();
        LinkStep::Continue
    }

    /// The close a fenced generation earns, if it is fenced.
    ///
    /// Superseded: the newer hello already asked this socket to close, so it
    /// closes with no code. Fenced with nothing replacing it: the credential was
    /// revoked under the socket, and v2 ends that socket `4001 revoked`.
    fn fenced_close(&mut self) -> Option<SocketClose> {
        if !self.handle.is_revoked() {
            return None;
        }
        self.keepalive.stop();
        let replaced = self
            .services
            .workers
            .current(&self.handle.worker_fp)
            .is_some();
        Some(if replaced {
            SocketClose::Default
        } else {
            SocketClose::Revoked
        })
    }

    /// Carry the kills this worker is owed, once its generation is routable.
    fn deliver_owed_reaps(&self) {
        if !self.handle.is_routable() {
            return;
        }
        for kill in self.reaps.take_pending() {
            match SessionId::try_from(kill.session_id.as_str()) {
                Ok(session_id) => {
                    reap_orphan_pty(&self.services.workers, &kill.worker_fp, &session_id);
                }
                Err(error) => tracing::warn!(worker_fp = %kill.worker_fp, %error,
                    "worker link: an owed reap names no addressable session"),
            }
        }
    }

    /// Offer the worker every open session it still owns, off the read loop.
    fn spawn_respawn(&self) {
        let services = Arc::clone(&self.services);
        let handle = Arc::clone(&self.handle);
        tokio::spawn(async move {
            if !handle.is_routable() {
                return;
            }
            let report = respawn_missing_for_worker(
                &services.db,
                &services.workers,
                &*services.views,
                &services.write_gate(),
                &handle,
            )
            .await;
            tracing::info!(worker_fp = %handle.worker_fp, dispatched = report.dispatched,
                skipped = report.skipped, deferred = report.deferred,
                "worker link: respawn-if-missing pass finished");
        });
    }

    /// Give the generation back (v2 `close`, and `revoke` for a fenced one).
    pub(super) fn end(mut self) {
        self.keepalive.stop();
        let was_revoked = self.handle.is_revoked();
        let services = Arc::clone(&self.services);
        if services.workers.retire_if_current(&self.handle) {
            publish_routable(&services.buses, &services.workers);
        }
        let replaced = services.workers.current(&self.handle.worker_fp).is_some();
        let end = if was_revoked && !replaced {
            LinkEnd::Revoked
        } else {
            LinkEnd::Closed { replaced }
        };
        // The owner index releases by fingerprint, so only a generation nothing
        // replaced may give the ownership back.
        if let Some(owner) = self.view_owner.take().filter(|_| !replaced) {
            owner.release();
        }
        services.worker_lifecycle.closed(&self.handle, end);
        tracing::info!(worker_fp = %self.handle.worker_fp, ?end, "worker link: closed");
    }

    /// The fingerprint this link was admitted under.
    pub(super) fn worker_fp(&self) -> &WorkerFp {
        &self.handle.worker_fp
    }

    /// The completion lane, whose backlog the read loop drains in order.
    pub(super) fn result_lane(&mut self) -> &mut ResultLane {
        &mut self.result_lane
    }
}

/// The handle's `send`: enqueue for the read loop, answering a non-zero
/// sequence while the loop still reads and zero once it has gone, which is the
/// transport's "dropped" (`worker_handle.rs`, v2's `ws.send`).
fn enqueue_into(
    outbound: mpsc::UnboundedSender<CoordWorkerDownstream>,
) -> Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync> {
    let sequence = AtomicI64::new(0);
    Arc::new(move |frame| {
        if outbound.send(frame).is_err() {
            return 0;
        }
        sequence.fetch_add(1, Ordering::AcqRel) + 1
    })
}
