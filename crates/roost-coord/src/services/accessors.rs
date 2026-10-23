//! Per-worker and per-write accessors on `CoordServices`: the `client_seq` cursor, the
//! worker frame dispatcher, and the write-gate handle. Called by the worker link and the
//! write interceptors. Depends on the `pub` fields of `CoordServices` in `services.rs`.

use std::sync::Arc;

use crate::worker_link::client_seq::ClientSeqCursor;
use crate::write_gate::WriteGate;

use super::CoordServices;

impl CoordServices {
    /// One worker's `client_seq` cursor, created on first use and shared by
    /// every socket for that fingerprint.
    ///
    /// An accessor rather than a public field read, because handing out the
    /// whole registry would let a caller iterate a worker's peers, and the one
    /// question this answers is "what is THIS worker's position".
    #[must_use]
    pub fn client_seq_cursor(&self, fingerprint: &str) -> Arc<ClientSeqCursor> {
        self.client_seqs.for_worker(fingerprint)
    }

    /// The frame dispatcher for one admitted worker socket.
    ///
    /// ON `Arc<Self>` RATHER THAN `&self`, because the dispatcher holds the
    /// services for as long as its socket lives and a borrowed `&self` could not
    /// become that. It is the ONE way a worker socket reaches the three dispatch
    /// arms, so a link that wanted a second dispatcher over a different event log
    /// would have to build one itself — which is the outcome this accessor
    /// exists to make unnecessary.
    #[must_use]
    pub fn worker_dispatcher(
        self: &Arc<Self>,
        handle: Arc<crate::coord_core::worker_handle::WorkerHandle>,
    ) -> crate::worker_link::frame_dispatch::WorkerFrameDispatcher {
        crate::worker_link::dispatcher_for::DispatcherFor::new(Arc::clone(self), handle).build()
    }

    /// A handle to the write gate, cloneable and shared.
    ///
    /// `WriteGate::clone` shares the gate rather than copying it, so a lease
    /// taken in a handler and released in an interceptor's `finally` refer to
    /// one counter even though they live in different frames.
    #[must_use]
    pub fn write_gate(&self) -> WriteGate {
        self.write_gate.clone()
    }
}
