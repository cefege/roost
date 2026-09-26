//! The one gate every coordinator→worker frame passes, and the three senders
//! the worker registry RPCs and the event path need.
//!
//! Ported from `apps/coord/src/workers/worker-send-target.ts` (the gate) and
//! the `sendBrowserCommand` arm of `apps/coord/src/workers/worker-send.ts:74-91`.
//!
//! WHY THE GATE IS HERE AND NOT IN THE HANDLE. v2 re-checks
//! `connectWorkers.get(workerFp) !== myHandle` inside the transport's own
//! `send`, because a browser command is sent from a router handler that resolved
//! the handle one await earlier. In Rust the identity check is this function's,
//! because the registry -- not the socket -- is what knows which generation is
//! current. The handle stays a dumb transport so a caller cannot forge one.
//!
//! A REFUSAL IS NAMED, NOT A BOOLEAN. v2 returns `false` and the caller logs
//! "worker offline". Three different things produce that `false` -- no socket, a
//! superseded generation, a transport that dropped the write -- and only the
//! second is a bug in the coordinator, so the reason travels with the answer.

use std::sync::Arc;

use roost_protocol::wire::SessionId;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};

/// The `browser_id` a coordinator-originated kill carries.
///
/// v2 uses `"coord-reap"` for both halves, and the worker treats them as opaque
/// strings it never learns from, so the pair only has to be stable and
/// distinguishable from a browser's own.
pub const COORD_REAP_BROWSER_ID: &str = "coord-reap";

/// The `viewer_id` a coordinator-originated kill carries.
pub const COORD_REAP_VIEWER_ID: &str = "coord-reap";

/// Why a frame did not reach a worker socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendRefusal {
    /// The worker has no current generation, or its current generation has not
    /// crossed its snapshot barrier or has been fenced.
    NoRoutableGeneration {
        /// The worker that was addressed.
        worker_fp: WorkerFp,
    },
    /// The handle is no longer the registry's current generation for its
    /// fingerprint: a reconnect or a retirement replaced it.
    SupersededGeneration {
        /// The worker that was addressed.
        worker_fp: WorkerFp,
        /// The generation the caller held.
        connection_generation: String,
    },
    /// The socket took the frame and dropped it, so nothing was written and
    /// nothing has mutated.
    TransportDropped {
        /// The worker that was addressed.
        worker_fp: WorkerFp,
        /// The generation the frame went to.
        connection_generation: String,
    },
}

impl std::fmt::Display for SendRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRoutableGeneration { worker_fp } => {
                write!(formatter, "worker {worker_fp} has no routable generation")
            }
            Self::SupersededGeneration {
                worker_fp,
                connection_generation,
            } => write!(
                formatter,
                "worker {worker_fp} generation {connection_generation} is no longer current"
            ),
            Self::TransportDropped {
                worker_fp,
                connection_generation,
            } => write!(
                formatter,
                "worker {worker_fp} generation {connection_generation} dropped the frame"
            ),
        }
    }
}

/// What a send decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// The socket accepted the frame; the value is its delivery sequence.
    Admitted {
        /// The socket's own sequence for the frame, which its replies echo.
        delivery_seq: i64,
    },
    /// The frame was not written. Nothing has mutated, so a retry cannot
    /// duplicate anything.
    Refused(SendRefusal),
}

impl SendOutcome {
    /// Whether the frame reached the socket.
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted { .. })
    }
}

/// The generation a frame may go to: ready, and not fenced.
///
/// The one place that predicate is written for the send path, so a caller cannot
/// resolve a handle by a weaker rule than the transport does.
#[must_use]
pub fn current_routable_worker(
    registry: &WorkerRegistry,
    worker_fp: &WorkerFp,
) -> Option<Arc<WorkerHandle>> {
    registry.current_routable(worker_fp)
}

/// Send one frame to the worker's current generation.
pub fn send_frame(
    registry: &WorkerRegistry,
    worker_fp: &WorkerFp,
    frame: CoordWorkerDownstream,
) -> SendOutcome {
    let Some(handle) = current_routable_worker(registry, worker_fp) else {
        return SendOutcome::Refused(SendRefusal::NoRoutableGeneration {
            worker_fp: worker_fp.clone(),
        });
    };
    admit(&handle, frame)
}

/// Send one frame through a handle the caller already holds.
///
/// The identity check is the whole point: a handler that resolved a generation
/// before an await must not deliver through it afterwards, and the only way to
/// know the generation it holds is stale is to ask the registry.
pub fn send_frame_through(
    registry: &WorkerRegistry,
    handle: &Arc<WorkerHandle>,
    frame: CoordWorkerDownstream,
) -> SendOutcome {
    let current = current_routable_worker(registry, &handle.worker_fp);
    let still_current = current
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, handle));
    if !still_current {
        return SendOutcome::Refused(SendRefusal::SupersededGeneration {
            worker_fp: handle.worker_fp.clone(),
            connection_generation: handle.connection_generation.clone(),
        });
    }
    admit(handle, frame)
}

/// Write one frame, and report what the socket said.
fn admit(handle: &Arc<WorkerHandle>, frame: CoordWorkerDownstream) -> SendOutcome {
    let delivery_seq = handle.send(frame);
    if delivery_seq == 0 {
        return SendOutcome::Refused(SendRefusal::TransportDropped {
            worker_fp: handle.worker_fp.clone(),
            connection_generation: handle.connection_generation.clone(),
        });
    }
    SendOutcome::Admitted { delivery_seq }
}

/// Relay one browser command to a worker.
///
/// The three ids travel separately because the worker echoes `request_id` and
/// treats the other two as opaque: which browser asked is not something a worker
/// learns, and multi-viewer presence needs no second channel for it.
pub fn send_browser_command(
    registry: &WorkerRegistry,
    worker_fp: &WorkerFp,
    browser_id: &str,
    viewer_id: &str,
    request_id: &str,
    frame: ClientControlFrame,
) -> SendOutcome {
    send_frame(
        registry,
        worker_fp,
        CoordWorkerDownstream::BrowserCommand {
            browser_id: browser_id.to_owned(),
            viewer_id: viewer_id.to_owned(),
            request_id: request_id.to_owned(),
            frame,
            trace_id: None,
        },
    )
}

/// Kill a PTY the coordinator force-closed while its worker was offline.
///
/// Fire and forget by design: the durable effective snapshot has already omitted
/// the session, so a failed kill cannot resurrect a route, and the next snapshot
/// re-offers the orphan if the PTY really is still there.
pub fn reap_orphan_pty(
    registry: &WorkerRegistry,
    worker_fp: &WorkerFp,
    session_id: &SessionId,
) -> SendOutcome {
    let request_id = reap_request_id(worker_fp, session_id);
    let outcome = send_browser_command(
        registry,
        worker_fp,
        COORD_REAP_BROWSER_ID,
        COORD_REAP_VIEWER_ID,
        &request_id,
        ClientControlFrame::Kill {
            session_id: session_id.clone(),
            trace_id: None,
        },
    );
    match &outcome {
        SendOutcome::Admitted { .. } => tracing::info!(
            %worker_fp, %session_id, "reap_orphan_kill_sent"
        ),
        SendOutcome::Refused(refusal) => {
            tracing::info!(%worker_fp, %session_id, %refusal, "reap_orphan_kill_no_socket");
        }
    }
    outcome
}

/// The correlation id a coordinator-originated kill carries.
///
/// v2 mints a fresh uuid per reap. The id is never consumed here -- the reap
/// registers no pending RPC, and the worker's reply is dropped -- so what it has
/// to be is unique among the frames in flight on one socket, which the
/// (worker, session) pair already is.
fn reap_request_id(worker_fp: &WorkerFp, session_id: &SessionId) -> String {
    format!("coord-reap:{worker_fp}:{session_id}")
}
