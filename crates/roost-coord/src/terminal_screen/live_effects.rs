//! The terminal half of what a committed session event does, once it is
//! durable.
//!
//! Owned by the terminal domain, because both methods act on terminal state and
//! `ByteHub` is the terminal domain's — which is the shape `WorkerLiveEffects`
//! already describes when it calls itself "the workers half, composed over a
//! terminal channel index".
//!
//! NEITHER METHOD IS A NO-OP, and that is the trait's rule rather than a
//! convention: a default body that silently does nothing is exactly the
//! history-corrupting drop this subsystem exists to prevent
//! (`events/append.rs:209-216`).
//!
//! `kill_orphan_pty` is PROMPT CLEANUP, NOT LOAD-BEARING, and the difference
//! matters to whoever reads it next. The durable effective snapshot has already
//! omitted the force-closed session ids, so a kill that never lands **cannot
//! resurrect a route** — that is what made a best-effort kill legal in the
//! first place. What a missed kill costs is a session row that outlives its
//! process. A reader who thinks the method is load-bearing will build for a
//! failure it cannot have; a reader who thinks it is optional will delete it.

use std::sync::Arc;

use roost_protocol::wire::{SessionEvent, WorkerFp};

use crate::coord_core::seams::WorkerRouteIndex;
use crate::events::append::LiveEffects;
use crate::terminal_screen::byte_hub::ByteHub;

/// The terminal side of the post-commit effects, over the byte hub's route cache
/// and the browser-command channel a reap travels on.
pub struct TerminalLiveEffects {
    /// The durable channel index. A cell frame that routes before the event
    /// that named its channel is a frame nobody can place.
    byte_hub: Arc<ByteHub>,
    /// Where a force-closed PTY's kill goes, for a worker that is offline now
    /// and will read it on reconnect.
    kills: Arc<dyn OrphanPtyKill>,
}

/// A log line needs to know WHICH collaborators are wired, and the kill
/// channel is a trait object — the same reason `CoordTerminal` writes its own
/// `Debug` rather than deriving one.
impl std::fmt::Debug for TerminalLiveEffects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalLiveEffects")
            .field("byte_hub", &std::any::type_name::<ByteHub>())
            .field("kills", &std::any::type_name::<dyn OrphanPtyKill>())
            .finish()
    }
}

/// Where `kill_orphan_pty` sends. A trait because the transport is the
/// worker's socket, which does not exist until the link lands, and a seam is
/// the only honest way to have the destination named before it is built.
pub trait OrphanPtyKill: Send + Sync {
    /// Kill the PTY behind `session_id` on `worker_fp`, or record that it has
    /// to be killed when that worker is reachable again.
    fn kill(&self, worker_fp: &WorkerFp, session_id: &str);
}

impl TerminalLiveEffects {
    /// The effects over a byte hub and a kill channel.
    #[must_use]
    pub fn new(byte_hub: Arc<ByteHub>, kills: Arc<dyn OrphanPtyKill>) -> Self {
        Self { byte_hub, kills }
    }
}

impl LiveEffects for TerminalLiveEffects {
    /// Apply the durable channel index for a committed event.
    ///
    /// THREE ARMS AND THEY ARE NOT INTERCHANGEABLE. `replace_worker_channel_index`
    /// takes a whole generation and REPLACES, so using it for a delta would wipe
    /// every other live channel on that worker — the exact history-corrupting
    /// drop this trait's own header forbids. `Snapshot` is the one arm that IS a
    /// generation; `Opened` and `Respawned` are deltas and bind one route.
    fn index_durable_channel(
        &self,
        event: &SessionEvent,
        authenticated_worker_fp: Option<&WorkerFp>,
    ) {
        match event {
            SessionEvent::Snapshot {
                worker_fp,
                sessions,
                ..
            } => {
                // The generation arm, and the only one that maps onto a
                // replacement: a snapshot is the worker's whole live index.
                let live = sessions
                    .iter()
                    .map(|session| crate::coord_core::seams::LiveChannel {
                        session_id: session.id.clone(),
                        channel_id: session.channel,
                    })
                    .collect::<Vec<_>>();
                self.byte_hub.replace_worker_channel_index(worker_fp, &live);
            }
            SessionEvent::Opened {
                session_id,
                worker_fp,
                channel,
                ..
            } => {
                // A DELTA. The event names its own worker, so this one binds
                // even when the producer is coordinator-side.
                self.byte_hub
                    .bind_durable_channel(worker_fp, *channel, session_id);
            }
            SessionEvent::Respawned {
                session_id,
                new_channel,
                ..
            } => {
                // A DELTA with NO worker in the event — the respawn is a rebind
                // of a session, not an announcement of a worker's index.
                //
                // **DOING NOTHING HERE IS THE CORRECT IMPLEMENTATION** when
                // `authenticated_worker_fp` is `None`, and that is the trait's
                // own rule: "`None` must bind nothing: inferring the worker from
                // the route cache could bind on a worker that has already been
                // replaced." A `None` means a coordinator-side producer, which
                // has no authenticated worker to attribute the rebind to.
                let Some(worker_fp) = authenticated_worker_fp else {
                    return;
                };
                self.byte_hub
                    .bind_durable_channel(worker_fp, *new_channel, session_id);
            }
            // Every other variant names no channel: `Closed`, `Detached` and
            // `Renamed` have no `ChannelId` to bind, and the durable `sessions`
            // row is what the reconciler reads for their state.
            _ => {}
        }
    }

    /// Kill a PTY the coordinator force-closed while its worker was offline.
    fn kill_orphan_pty(&self, worker_fp: &WorkerFp, session_id: &str) {
        self.kills.kill(worker_fp, session_id);
    }
}
