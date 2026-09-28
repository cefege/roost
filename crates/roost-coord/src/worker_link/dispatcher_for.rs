//! What one worker socket's dispatcher is bound to, and the factory that binds it.
//!
//! A dispatcher is per SOCKET: v2 builds one per connection inside
//! `worker-conn.ts`, and so does this. Everything it reaches — the event log, the
//! byte hub, the pending-RPC table, the view hub, the agent-status hub — is per
//! PROCESS, so it arrives as one `Arc<CoordServices>`. What is per socket arrives
//! as that socket's own `WorkerHandle`: the readiness barrier, the generation
//! fence, and the sender the acknowledgements travel on. Two arguments, because
//! every other collaborator is either process state or a field of the handle, and
//! a third would be a second place for a future collaborator to hide in.

use std::sync::Arc;

use crate::coord_core::core::CoordCore;
use crate::coord_core::worker_handle::WorkerHandle;
use crate::services::CoordServices;
use crate::worker_link::frame_dispatch::WorkerFrameDispatcher;

/// A dispatcher for one authenticated worker socket, not yet built.
///
/// The name is the grammar the module is about: a dispatcher FOR a socket, as
/// opposed to the dispatcher a process would have if it had one. It has no
/// `Default` and no zero-argument constructor, because a dispatcher with no
/// handle has no fence and no sender and would answer every question with a
/// silent drop — the exact shape of the defect this slice exists to close.
#[derive(Debug)]
pub struct DispatcherFor {
    /// The process state, wrapped so the agent-status hub can be reached the
    /// way its one caller reaches it.
    core: CoordCore,
    /// THIS socket's generation. Never another worker's, never a fresh one.
    handle: Arc<WorkerHandle>,
}

impl DispatcherFor {
    /// Bind the process state and one socket's own handle.
    ///
    /// THE HANDLE MUST ALREADY BE THIS SOCKET'S. It is what makes the generation
    /// fence and the readiness barrier answerable: both are questions about a
    /// particular generation, and a dispatcher built over another generation's
    /// handle would fence against the wrong socket and could mark a superseded
    /// worker routable. `worker_link::connection` builds the handle, claims the
    /// generation, and only then asks for a dispatcher.
    #[must_use]
    pub fn new(services: Arc<CoordServices>, handle: Arc<WorkerHandle>) -> Self {
        // A fresh `CoordCore` over the same services, NOT the one the RPC
        // surface holds. `accept_worker_status` reads `core.services.byte_hub`
        // and nothing else from the core, and borrowing the RPC core would tie
        // the link's lifetime to a value built per request.
        Self {
            core: CoordCore::new(services),
            handle,
        }
    }

    /// The one dispatcher for this socket.
    ///
    /// The `client_seq` cursor is resolved HERE rather than per frame, because it
    /// is per WORKER and outlives the socket: a reconnect resumes the outbox, so
    /// a cursor rebuilt per connection would restart at zero and the coordinator
    /// would read the replayed sequence 1 as a repeat.
    #[must_use]
    pub fn build(self) -> WorkerFrameDispatcher {
        let cursor = self
            .core
            .services
            .client_seq_cursor(self.handle.worker_fp.as_str());
        WorkerFrameDispatcher::new(self.core, self.handle, cursor)
    }
}
