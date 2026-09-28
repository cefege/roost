//! How the worker link's transitions reach terminal-peer signaling: a hello is
//! acknowledged `terminal-peer-webrtc-v1` only while the carrier is enabled and
//! this owner exists, and a superseded, revoked or closed generation's pending
//! offers fail at once instead of waiting out their answer deadline.
//! Registered on `CoordServices::worker_lifecycle`. Ports the terminal-peer calls
//! `apps/coord/src/workers/worker-conn.ts` makes at hello, supersede, revoke and close.

use std::collections::BTreeSet;
use std::sync::Arc;

use roost_protocol::versioning::CAPABILITY_TERMINAL_PEER_WEBRTC_V1;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::coord_core::worker_lifecycle::{LinkEnd, WorkerLifecycleObserver};
use crate::terminal_direct::peer_negotiations::TerminalPeerNegotiations;

impl WorkerLifecycleObserver for TerminalPeerNegotiations {
    fn acknowledge_capabilities(&self, advertised: &BTreeSet<String>) -> Vec<&'static str> {
        if self.settings().enabled && advertised.contains(CAPABILITY_TERMINAL_PEER_WEBRTC_V1) {
            vec![CAPABILITY_TERMINAL_PEER_WEBRTC_V1]
        } else {
            Vec::new()
        }
    }

    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.cancel_for_worker_handle(superseded, "connection_superseded");
    }

    fn on_closed(&self, handle: &Arc<WorkerHandle>, end: LinkEnd) {
        let reason = match end {
            LinkEnd::Revoked => "worker_revoked",
            LinkEnd::Closed { .. } => "worker_disconnected",
        };
        self.cancel_for_worker_handle(handle, reason);
    }
}
