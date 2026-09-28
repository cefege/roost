//! The fences every terminal-peer negotiation passes: an exact owned grant, a
//! grant that did not change while authorization or the answer was awaited,
//! the exact current worker generation, and a typed result that answers the
//! offer that was actually sent.
//! Called by `peer_negotiations`. Ports `requireOwnedGrant`, `requireStableGrant`,
//! `requireWorker` and `matchesPending` of `apps/coord/src/terminal/direct/terminal-peer-negotiations.ts`.

use std::sync::Arc;

use connectrpc::ConnectError;
use roost_proto::SessionsNegotiateLocalTerminalPeerRequest as PeerRequest;
use roost_protocol::versioning::CAPABILITY_TERMINAL_PEER_WEBRTC_V1;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_direct::grant_owner::current_routable;
use crate::terminal_direct::grant_state::TerminalGrantLeaseSnapshot;
use crate::terminal_direct::peer_negotiations::TerminalPeerNegotiations;
use crate::terminal_direct::peer_state::{
    has_valid_terminal_peer_grant_sessions, terminal_peer_denied, terminal_peer_unavailable,
};
use crate::terminal_direct::peer_table::PendingTerminalPeerNegotiation;
use crate::workers::terminal_peer_send::is_current_terminal_peer_worker;

impl TerminalPeerNegotiations {
    /// The caller's exact live lease, over a session list a peer may cover.
    pub(super) fn require_owned_grant(
        &self,
        owner_key: &str,
        request: &PeerRequest,
    ) -> Result<TerminalGrantLeaseSnapshot, ConnectError> {
        self.grants
            .owned_grant(
                owner_key,
                &request.tab_id,
                &request.worker_fp,
                &request.grant_id,
            )
            .filter(|grant| has_valid_terminal_peer_grant_sessions(&grant.session_ids))
            .ok_or_else(|| terminal_peer_denied("terminal peer grant is unavailable"))
    }

    /// The lease again, refused if its id, generation, epoch or sessions moved.
    pub(super) fn require_stable_grant(
        &self,
        owner_key: &str,
        request: &PeerRequest,
        expected: &TerminalGrantLeaseSnapshot,
    ) -> Result<TerminalGrantLeaseSnapshot, ConnectError> {
        let current = self.require_owned_grant(owner_key, request)?;
        if current.grant_id != expected.grant_id
            || !Arc::ptr_eq(&current.worker_handle, &expected.worker_handle)
            || current.worker_epoch != expected.worker_epoch
            || current.session_ids != expected.session_ids
        {
            return Err(terminal_peer_denied(
                "terminal peer grant changed during negotiation",
            ));
        }
        Ok(current)
    }

    /// The worker generation the lease names, if it is current, on the
    /// request's epoch, and acknowledged for the peer carrier.
    pub(super) fn require_worker(
        &self,
        request: &PeerRequest,
        grant: &TerminalGrantLeaseSnapshot,
    ) -> Result<Arc<WorkerHandle>, ConnectError> {
        let worker = current_routable(&self.workers, &request.worker_fp)
            .filter(|worker| self.settings.enabled && Arc::ptr_eq(worker, &grant.worker_handle))
            .filter(|worker| {
                worker.process_epoch.is_some()
                    && worker.process_epoch == grant.worker_epoch
                    && worker.process_epoch.as_deref() == Some(request.worker_epoch.as_str())
            })
            .filter(|worker| {
                worker
                    .capabilities
                    .contains(CAPABILITY_TERMINAL_PEER_WEBRTC_V1)
                    && is_current_terminal_peer_worker(&self.workers, worker, &request.worker_epoch)
            });
        worker.ok_or_else(|| terminal_peer_unavailable("terminal peer worker is unavailable"))
    }

    /// Whether a typed result came from the exact generation the offer went to
    /// and names that offer's generation, epoch and peer.
    pub(super) fn matches_pending(
        &self,
        source: &Arc<WorkerHandle>,
        pending: &PendingTerminalPeerNegotiation,
        connection_generation: &str,
        worker_epoch: &str,
        peer_id: &str,
    ) -> bool {
        Arc::ptr_eq(source, &pending.worker)
            && source.connection_generation == pending.connection_generation
            && is_current_terminal_peer_worker(&self.workers, source, &pending.worker_epoch)
            && connection_generation == pending.connection_generation
            && worker_epoch == pending.worker_epoch
            && peer_id == pending.peer_id
    }
}
