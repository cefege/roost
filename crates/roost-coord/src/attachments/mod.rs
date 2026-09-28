//! The attachments domain: the file RPCs and the chunk relay, and the direct
//! transfer's three owners -- the grant registry, the peer negotiations that
//! read it, and the receipt-status correlation.
//!
//! One field on `CoordServices`, reached as `core.services.attachments`. Every
//! owner is built once here, so the peer owner subscribes to the one grant
//! registry the RPCs mint into, and the worker link settles and fences the same
//! tables the RPCs wait on (v2 composes the same three in `coord-factory.ts`).
//!
//! `new` takes the process's worker registry and pending-request table and
//! nothing from configuration: what the domain needs from the operator's config
//! is read at call time from `core.services.boot`.

use std::sync::Arc;

use crate::coord_core::worker_handle::WorkerRegistry;
use crate::coord_core::worker_lifecycle::WorkerLifecycleObserver;
use crate::terminal_screen::pending_rpcs::PendingRpcs;

pub mod files;
pub mod grant;
pub mod grant_state;
pub(crate) mod grant_table;
pub mod peer;
pub mod peer_state;
pub(crate) mod peer_table;
pub(crate) mod relay;
pub mod rpc_direct;
pub mod rpc_files;
pub mod session_files;
pub mod status_results;
pub mod transfer_limits;
pub mod worker_conn;

use grant::AttachmentGrantOwner;
use grant_state::AttachmentGrantPort;
use peer::AttachmentPeerNegotiations;
use status_results::AttachmentDirectStatusResults;

/// The attachment state one coordinator process holds.
#[derive(Debug)]
pub struct AttachmentsRuntime {
    grants: Arc<AttachmentGrantOwner>,
    peers: Arc<AttachmentPeerNegotiations>,
    statuses: Arc<AttachmentDirectStatusResults>,
}

impl AttachmentsRuntime {
    /// A coordinator with no grants, negotiations or status requests.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, pending_rpcs: Arc<PendingRpcs>) -> Self {
        let grants = AttachmentGrantOwner::new(Arc::clone(&workers), pending_rpcs);
        let port = Arc::clone(&grants) as Arc<dyn AttachmentGrantPort>;
        Self {
            peers: AttachmentPeerNegotiations::new(Arc::clone(&workers), port),
            statuses: Arc::new(AttachmentDirectStatusResults::new(workers)),
            grants,
        }
    }

    /// The direct grant registry.
    #[must_use]
    pub fn grants(&self) -> &Arc<AttachmentGrantOwner> {
        &self.grants
    }

    /// The attachment-peer signaling owner.
    #[must_use]
    pub fn peers(&self) -> &Arc<AttachmentPeerNegotiations> {
        &self.peers
    }

    /// The receipt-status correlation owner.
    #[must_use]
    pub fn statuses(&self) -> &Arc<AttachmentDirectStatusResults> {
        &self.statuses
    }

    /// The observer that fails this domain's typed results when a worker
    /// generation ends; registered on `CoordServices::worker_lifecycle`.
    #[must_use]
    pub fn worker_lifecycle_observer(&self) -> Arc<dyn WorkerLifecycleObserver> {
        Arc::new(worker_conn::AttachmentWorkerLifecycle::new(
            Arc::clone(&self.peers),
            Arc::clone(&self.statuses),
        ))
    }
}
