//! The worker's downstream owners, built ONCE over the session stack, and the
//! shared handles later owners reuse (input routes, the input work budget, the
//! view owner, the cell cadence, the uplink, the coordinator cell sink and the
//! stack itself). Called by `runtime::boot_sequence::run` only. Ports the
//! composition half of v2 `apps/worker/src/main.ts:156-237` and
//! `transport/coord-link-deps.ts:76-181`.

use std::sync::Arc;

use roost_observability::clock::EventClock;

use super::agent_owners::start_agent_status;
use super::cell_cadence::CellCadence;
use super::heart_owners::HeartOwners;
use super::link_loop::CoordinatorCellSink;
use super::link_wire::ProtoLinkWire;
use super::reconcile_gate::{ReconcileGate, ReconcileInputs};
use super::session_stack::SessionStack;
use crate::agents::report_server::AgentReportServer;
use crate::agents::status_stack::AgentStatusStack;
use crate::attachments::owners::{AttachmentOwners, AttachmentOwnersDeps};
use crate::attachments::peer_owner::AttachmentPeerBootstrapState;
use crate::door::loopback::{LoopbackOwner, LoopbackRoutes};
use crate::keeper_pool::{KeeperPool, KeeperUpdateBoundary, KeeperUpdatePreparer};
use crate::link_ports::{
    AttachmentPeerPort, DirectTerminalPort, DownstreamOwners, KeeperUpdatePort, LinkLifecyclePort,
    LinkLifecycles, LocalTerminalGrantPort, TerminalInputPort, TerminalPipelinePort,
    TerminalStreamPort, TerminalViewPort,
};
use crate::local_terminal::{LocalTerminalDoor, LocalTerminalDoorDeps};
use crate::peer::native::NativeLoader;
use crate::peer::{
    DirectLinkLifecycle, DirectPeerSupport, DirectTerminal, DirectTerminalDeps, PeerBootstrapState,
    PeerTransportConfig,
};
use crate::session::cwd_events::CwdEventLane;
use crate::session::query_reply::QueryReplyLane;
use crate::session::terminal_stream_owner::StreamOwner;
use crate::terminal_input::{InputOwner, TerminalInputRouteOwner, TerminalInputWorkBudget};
use crate::terminal_pipeline::{KeeperPipelineSource, PipelineOwner};
use crate::terminal_view::{SessionViewPort, TerminalViewOwner, TerminalViewOwnerDeps};
use crate::uplink::Uplink;

/// Everything boot composes around the session stack, for the life of the link.
pub struct WorkerOwners {
    /// The session layer, moved in so a later owner reaches it through here.
    pub stack: SessionStack,
    /// What `LinkLoop::attach_owners` routes downstream frames to.
    pub downstream: DownstreamOwners,
    /// v2 `TerminalInputRouteOwner`, shared with the local door.
    pub routes: TerminalInputRouteOwner,
    /// v2 `TerminalInputWorkBudget`, shared with the local door.
    pub work_budget: TerminalInputWorkBudget,
    /// v2 `TerminalViewOwner`; the door registers local sockets on it.
    pub view: Arc<TerminalViewOwner>,
    /// v2 `LocalTerminalDoor.wiring`: the grant store and direct socket owner;
    /// the door's router serves `local_terminal.sockets()`.
    pub local_terminal: Arc<LocalTerminalDoor>,
    /// v2 `boot-local-terminal.ts`'s peer half: the terminal peer owner, the
    /// coordinator generation and the direct carriers retired together.
    pub direct: Arc<DirectTerminal>,
    /// The ONE native peer load both peer owners share.
    pub native_loader: NativeLoader,
    /// The cell driver; the door registers its cell sinks on it.
    pub cadence: CellCadence,
    /// The one way anything off the link loop puts a frame on the link.
    pub uplink: Uplink,
    /// The coordinator's cell sink: the SAME `Arc` the cadence registered and
    /// `LinkLoop::attach_cell_sink` drains.
    pub coord_sink: Arc<CoordinatorCellSink>,
    /// Heartbeat, folder facts, stray sweeper and reconciliation stamp.
    pub heart: HeartOwners,
    /// v2 `setupReconcile`: the one door for boot, keeper-death and degraded passes.
    pub reconcile: ReconcileGate,
    /// v2 `agentRegistry` + `agentDetector`, with the manifests and the ONE
    /// reference admission gate the report server and prompt owner share.
    pub agents: AgentStatusStack,
    /// v2 `agentReportServer`: integrations report status and references over
    /// it; `None` when it could not start. Closed by `close_agent_report`.
    agent_report: Option<AgentReportServer>,
    /// v2 `main.ts:196-199` + `boot-local-terminal.ts`: the attachment
    /// operation owner, grant store, direct carriers and their sweeps.
    pub attachments: AttachmentOwners,
    cadence_task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for WorkerOwners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerOwners")
            .field("stack", &self.stack)
            .field("routes", &self.routes)
            .field("coord_sink", &self.coord_sink)
            .finish_non_exhaustive()
    }
}

impl WorkerOwners {
    /// Build every wave-1 owner over `stack`, start the cell cadence and the
    /// query-reply writer, and register the session-closed hook. Must run
    /// inside the worker's tokio runtime.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        stack: SessionStack,
        uplink: &Uplink,
        process_epoch: &str,
        pool: Arc<KeeperPool>,
        worker_fingerprint: &str,
        platform: roost_host::HostPlatform,
        transport: PeerTransportConfig,
        reconcile: ReconcileInputs,
    ) -> anyhow::Result<Self> {
        let clock: Arc<dyn EventClock> = stack.clock.clone();

        // v2 `main.ts:211-217`: the coordinator is one cell sink among several,
        // registered on the one emitter by the cadence that drives it.
        let coord_sink = Arc::new(CoordinatorCellSink::new(Arc::new(ProtoLinkWire)));
        let (cadence, cadence_task) = CellCadence::spawn(
            Arc::clone(&stack.emitter),
            Arc::clone(&stack.table),
            Arc::clone(&clock),
            uplink.clone(),
            Arc::clone(&coord_sink),
        );

        // Terminal query replies are written back to their PTY by one writer,
        // in order per session; an OSC 7 folder change is published as a `cwd`
        // session event by another.
        let (replies, reply_writer) = QueryReplyLane::new();
        let (cwd_events, cwd_writer) = CwdEventLane::new();
        {
            let mut emitter = stack
                .emitter
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            emitter.attach_query_replies(replies);
            emitter.attach_cwd_events(cwd_events);
        }
        tokio::spawn(reply_writer.run(Arc::clone(&stack.manager)));
        tokio::spawn(cwd_writer.run(Arc::clone(&stack.manager)));

        let work_budget = TerminalInputWorkBudget::new();
        let routes = TerminalInputRouteOwner::new(
            process_epoch.to_owned(),
            Arc::clone(&stack.table),
            Arc::clone(stack.manager.control_lanes()),
            work_budget.clone(),
        );
        let input = InputOwner::new(
            Arc::clone(&stack.manager),
            Arc::clone(&stack.table),
            routes.clone(),
            work_budget.clone(),
        );
        let view = TerminalViewOwner::new(TerminalViewOwnerDeps {
            sessions: Arc::new(SessionViewPort::new(
                Arc::clone(&stack.manager),
                Arc::clone(&stack.table),
                cadence.clone(),
            )),
            uplink: uplink.clone(),
            clock: Arc::clone(&clock),
            runtime: tokio::runtime::Handle::current(),
        });
        // v2 `boot-local-terminal.ts`: the direct path shares the link's view
        // owner, input routes and work budget rather than owning copies.
        let local_terminal = LocalTerminalDoor::new(LocalTerminalDoorDeps {
            manager: Arc::clone(&stack.manager),
            sessions: Arc::clone(&stack.table),
            view: Arc::clone(&view),
            routes: routes.clone(),
            work_budget: work_budget.clone(),
            worker_fingerprint: worker_fingerprint.to_owned(),
            process_epoch: process_epoch.to_owned(),
            runtime: tokio::runtime::Handle::current(),
        });
        #[cfg(feature = "smoke")]
        let fault_controls =
            crate::smoke_faults::FaultControls::attach(&reconcile.boot, &local_terminal);
        let native_loader = crate::peer::native::str0m_loader();
        let direct = DirectTerminal::new(DirectTerminalDeps {
            door: Arc::clone(&local_terminal),
            process_epoch: process_epoch.to_owned(),
            transport,
            native_loader: native_loader.clone(),
            #[cfg(feature = "smoke")]
            test_faults: fault_controls
                .as_ref()
                .map(crate::smoke_faults::FaultControls::peer_faults),
            #[cfg(not(feature = "smoke"))]
            test_faults: None,
            runtime: tokio::runtime::Handle::current(),
        });
        let attachments = AttachmentOwners::start(AttachmentOwnersDeps {
            base: stack.attachments.clone(),
            process_epoch: process_epoch.to_owned(),
            worker_fingerprint: worker_fingerprint.to_owned(),
            peer: transport,
            native_loader: native_loader.clone(),
            coordinator_generation: direct.generation(),
        });
        // v2 `coord-link-deps.ts:154-156`, `disposeDirect`: the attachment
        // carriers are fenced on detach and retired with the terminal ones.
        direct.register_carrier(Arc::new(attachments.direct.clone()));
        let heart = HeartOwners::build(&stack, Arc::clone(&pool), platform);
        // v2 `main.ts:220-237`: agent-status detection, hooked to terminal
        // output and to session close ahead of the routes and view. Built
        // before the reconcile gate, which shares its reference admission gate.
        let agents = start_agent_status(&stack, uplink)?;
        let keeper_endpoint = crate::runtime::keeper_boot::keeper_endpoint(&reconcile.boot)?;
        let reconcile = ReconcileGate::start(
            &stack,
            &pool,
            &heart,
            reconcile,
            agents.reference_admission.clone(),
        );
        // v2 `onKeeperUpdatePrepare` joins the same reconcile boundary boot
        // and keeper-death reconciliation serialize on.
        let keeper_update = KeeperUpdatePreparer::over_pool(
            Arc::clone(&stack.manager),
            Arc::clone(&stack.table),
            Arc::new(reconcile.clone()) as Arc<dyn KeeperUpdateBoundary>,
            Arc::clone(&pool),
            keeper_endpoint,
        );
        let keeper: Arc<dyn KeeperPipelineSource> = pool;
        let pipeline = PipelineOwner::new(
            Arc::clone(&stack.table),
            Arc::clone(&stack.manager),
            Arc::clone(&stack.emitter),
            clock,
            keeper,
        );

        let agent_report = AgentReportServer::start_for_worker(
            &stack.agent_environment,
            &agents,
            stack.manager.durable_event_sink(),
        );
        register_session_closed(&stack, &routes, &view);

        let downstream = DownstreamOwners {
            input: Arc::new(input) as Arc<dyn TerminalInputPort>,
            stream: Arc::new(StreamOwner::new(Arc::clone(&stack.manager)))
                as Arc<dyn TerminalStreamPort>,
            pipeline: Arc::new(pipeline) as Arc<dyn TerminalPipelinePort>,
            view: Arc::clone(&view) as Arc<dyn TerminalViewPort>,
            // v2 `onSnapshotReady` reaches the direct path and cell sink, then
            // `agentRegistry.resend()` (`coord-link-deps.ts:176`).
            lifecycle: Arc::new(LinkLifecycles::new(vec![
                Arc::new(DirectLinkLifecycle::new(
                    Arc::clone(&direct),
                    Arc::new(cadence.clone()),
                )) as Arc<dyn LinkLifecyclePort>,
                Arc::new(agents.clone()) as Arc<dyn LinkLifecyclePort>,
            ])) as Arc<dyn LinkLifecyclePort>,
            // v2 `wiring.revokeDevice`: the door's routes and grants, then the
            // device's peers.
            local_terminal: Arc::clone(&direct) as Arc<dyn LocalTerminalGrantPort>,
            direct: Some(Arc::clone(&direct) as Arc<dyn DirectTerminalPort>),
            // v2 `onAgentPrompt` (`coord-link-deps.ts:218-249`).
            agent_prompt: crate::agents::prompt_port::AgentPromptOwner::over_status_stack(
                Arc::clone(&stack.manager),
                Arc::clone(&stack.table),
                &agents,
                work_budget.clone(),
            ),
            attachments: attachments.link(),
            attachment_peers: Some(
                Arc::new(attachments.direct.clone()) as Arc<dyn AttachmentPeerPort>
            ),
            keeper_update: Arc::new(keeper_update) as Arc<dyn KeeperUpdatePort>,
        };
        tracing::info!(
            "the downstream owners are built: input, stream, pipeline, view and the cell cadence"
        );
        // Last, so every owner a fault command reaches already exists.
        #[cfg(feature = "smoke")]
        if let Some(controls) = fault_controls {
            let targets = crate::smoke_faults::FaultTargets {
                direct: Arc::clone(&direct),
                grants: local_terminal.grants(),
                admission: crate::smoke_faults::AdmissionHolds::new(
                    Arc::clone(&stack.table),
                    Arc::clone(stack.manager.control_lanes()),
                ),
            };
            controls.serve_commands(&tokio::runtime::Handle::current(), targets);
        }
        Ok(Self {
            stack,
            downstream,
            routes,
            work_budget,
            view,
            local_terminal,
            direct,
            native_loader,
            cadence,
            uplink: uplink.clone(),
            coord_sink,
            heart,
            reconcile,
            agents,
            agent_report,
            attachments,
            cadence_task,
        })
    }

    /// The owners the loopback door upgrades sockets into (v2 `startLocalUiServer`'s
    /// `terminal` and `attachment`).
    pub fn loopback_routes(&self) -> LoopbackRoutes {
        LoopbackRoutes {
            terminal: LoopbackOwner::terminal(self.local_terminal.sockets()),
            attachment: Some(LoopbackOwner::attachment(Arc::new(
                self.attachments.direct.sockets(),
            ))),
        }
    }

    /// v2 `main.ts:348-354`: on shutdown the report server stops accepting,
    /// lets open admissions finish, and removes its socket — before the door
    /// closes. Dropping it without this only stops the accept loop.
    pub async fn close_agent_report(&mut self) {
        if let Some(server) = self.agent_report.take() {
            server.close().await;
        }
    }

    /// What the hello may promise: a peer capability only for an owner whose
    /// native bootstrap is `ready` (v2 `boot-local-terminal.ts:131-174`).
    pub async fn direct_peer_support(&self) -> DirectPeerSupport {
        DirectPeerSupport {
            terminal: self.direct.peer_owner().bootstrap().await == PeerBootstrapState::Ready,
            attachment: self.attachments.direct.bootstrap().await
                == AttachmentPeerBootstrapState::Ready,
        }
    }

    /// Release what the owners hold at worker shutdown: the view owner's
    /// sockets and sweep, every input route and work reservation, and the
    /// cadence task. The keeper and its PTYs are untouched.
    pub fn shutdown(self) {
        self.heart.shutdown();
        // v2 `main.ts:363-364`: the detector, then the registry.
        self.agents.dispose();
        self.attachments.shutdown();
        // v2 `close()`: the direct carriers and the door's sockets go before
        // the view owner; `dispose_direct` disposes the door once.
        self.direct.dispose_direct();
        self.view.dispose();
        self.routes.dispose();
        self.work_budget.dispose();
        self.cadence_task.abort();
        // v2 `session-lifecycle.ts:314` `stopTerminalCaptureMaintenance`.
        self.stack.capture.stop_maintenance();
        tracing::info!("the downstream owners were disposed and the cell cadence stopped");
    }
}

/// v2 `main.ts:228-236`: a closed PTY retires its input routes and drops its
/// view membership and stream identity instead of holding a dead geometry.
fn register_session_closed(
    stack: &SessionStack,
    routes: &TerminalInputRouteOwner,
    view: &Arc<TerminalViewOwner>,
) {
    let routes = routes.clone();
    let view = Arc::downgrade(view);
    stack.manager.on_session_closed(Arc::new(move |session_id| {
        routes.retire_session(session_id.as_str());
        if let Some(view) = view.upgrade() {
            view.close_session(session_id);
        }
    }));
}
