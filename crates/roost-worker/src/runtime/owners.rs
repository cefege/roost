//! The worker's downstream owners, built ONCE over the session stack, and the
//! shared handles later owners reuse (input routes, the input work budget, the
//! view owner, the cell cadence, the uplink, the coordinator cell sink and the
//! stack itself). Called by `runtime::boot_sequence::run` only. Ports the
//! composition half of v2 `apps/worker/src/main.ts:156-237` and
//! `transport/coord-link-deps.ts:76-181`.

use std::sync::Arc;

use roost_observability::clock::EventClock;

use super::cell_cadence::CellCadence;
use super::link_loop::CoordinatorCellSink;
use super::link_wire::ProtoLinkWire;
use super::session_stack::SessionStack;
use crate::keeper_pool::KeeperPool;
use crate::link_ports::{
    DownstreamOwners, LinkLifecyclePort, TerminalInputPort, TerminalPipelinePort,
    TerminalStreamPort, TerminalViewPort,
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
    /// The cell driver; the door registers its cell sinks on it.
    pub cadence: CellCadence,
    /// The one way anything off the link loop puts a frame on the link.
    pub uplink: Uplink,
    /// The coordinator's cell sink: the SAME `Arc` the cadence registered and
    /// `LinkLoop::attach_cell_sink` drains.
    pub coord_sink: Arc<CoordinatorCellSink>,
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
    pub fn build(
        stack: SessionStack,
        uplink: &Uplink,
        process_epoch: &str,
        pool: Arc<KeeperPool>,
    ) -> Self {
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
        let keeper: Arc<dyn KeeperPipelineSource> = pool;
        let pipeline = PipelineOwner::new(
            Arc::clone(&stack.table),
            Arc::clone(&stack.manager),
            Arc::clone(&stack.emitter),
            clock,
            keeper,
        );

        register_session_closed(&stack, &routes, &view);

        let downstream = DownstreamOwners {
            input: Arc::new(input) as Arc<dyn TerminalInputPort>,
            stream: Arc::new(StreamOwner::new(Arc::clone(&stack.manager)))
                as Arc<dyn TerminalStreamPort>,
            pipeline: Arc::new(pipeline) as Arc<dyn TerminalPipelinePort>,
            view: Arc::clone(&view) as Arc<dyn TerminalViewPort>,
            lifecycle: Arc::new(cadence.clone()) as Arc<dyn LinkLifecyclePort>,
        };
        tracing::info!(
            "the downstream owners are built: input, stream, pipeline, view and the cell cadence"
        );
        Self {
            stack,
            downstream,
            routes,
            work_budget,
            view,
            cadence,
            uplink: uplink.clone(),
            coord_sink,
            cadence_task,
        }
    }

    /// Release what the owners hold at worker shutdown: the view owner's
    /// sockets and sweep, every input route and work reservation, and the
    /// cadence task. The keeper and its PTYs are untouched.
    pub fn shutdown(self) {
        self.view.dispose();
        self.routes.dispose();
        self.work_budget.dispose();
        self.cadence_task.abort();
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
