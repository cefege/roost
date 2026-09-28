//! The direct-terminal domain: the grant leases a browser's loopback or WebRTC
//! terminal authenticates with, and the signaling that negotiates a WebRTC peer
//! over one. `TerminalDirectRuntime` is built once on
//! `CoordServices::terminal_direct`; its negotiations are also a worker
//! lifecycle observer. Ports the `TerminalGrantOwner`/`TerminalPeerNegotiations`
//! composition of `apps/coord/src/coord-factory.ts` and `main.ts`'s revocation hook.

use std::num::NonZeroU64;
use std::sync::Arc;

use roost_host::CoordConfig;
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS;

use crate::coord_core::worker_handle::WorkerRegistry;
use crate::db::CoordDb;
use crate::terminal_screen::pending_rpcs::PendingRpcs;

pub mod grant_owner;
pub mod grant_refresh;
pub mod grant_rpc;
pub mod grant_state;
mod peer_fences;
mod peer_lifecycle;
pub mod peer_negotiations;
pub mod peer_rpc;
mod peer_settle;
pub mod peer_state;
pub(crate) mod peer_table;

use grant_owner::TerminalGrantOwner;
pub use grant_state::TerminalDirectRetireReason;
use peer_negotiations::{TerminalPeerNegotiations, TerminalPeerNegotiationsOptions};
use peer_state::{TerminalGrantSessionAuthorizer, TerminalPeerGrantPort, TerminalPeerSettings};

/// The process's direct-terminal state.
#[derive(Debug)]
pub struct TerminalDirectRuntime {
    grants: Arc<TerminalGrantOwner>,
    negotiations: Arc<TerminalPeerNegotiations>,
}

impl TerminalDirectRuntime {
    /// A runtime over the process's worker registry, pending-request table and
    /// database. The peer settings are read from the boot config once, because
    /// the worker link's hello acknowledgement consults them without a services
    /// handle and boot facts never change after `CoordServices::booted`.
    #[must_use]
    pub fn new(
        workers: Arc<WorkerRegistry>,
        pending_rpcs: Arc<PendingRpcs>,
        database: CoordDb,
        config: Option<&CoordConfig>,
    ) -> Self {
        let grants = TerminalGrantOwner::new(Arc::clone(&workers), pending_rpcs);
        let authorize_sessions: TerminalGrantSessionAuthorizer =
            Arc::new(move |worker_fp: String, session_ids: Vec<String>| {
                let database = database.clone();
                Box::pin(async move {
                    grant_rpc::authorize_terminal_grant_sessions(
                        &database,
                        &worker_fp,
                        &session_ids,
                    )
                    .await
                })
            });
        let negotiations = TerminalPeerNegotiations::new(TerminalPeerNegotiationsOptions {
            workers,
            grants: Arc::clone(&grants) as Arc<dyn TerminalPeerGrantPort>,
            settings: TerminalPeerSettings::from_config(config),
            authorize_sessions,
            answer_timeout_ms: NonZeroU64::new(TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS)
                .unwrap_or(NonZeroU64::MIN),
        });
        Self {
            grants,
            negotiations,
        }
    }

    /// The lease registry.
    #[must_use]
    pub fn grants(&self) -> &Arc<TerminalGrantOwner> {
        &self.grants
    }

    /// The peer signaling owner; also a worker lifecycle observer.
    #[must_use]
    pub fn negotiations(&self) -> &Arc<TerminalPeerNegotiations> {
        &self.negotiations
    }

    /// Retire a deleted or revoked worker's direct transport before its
    /// generation is fenced. A failure is logged, never propagated: the caller's
    /// irreversible commit already happened (v2 `handlers-workers.ts:210-218`).
    pub fn retire_worker(&self, worker_fp: &str, reason: TerminalDirectRetireReason) {
        if let Err(error) = self.grants.retire_worker(worker_fp, reason) {
            tracing::warn!(worker_fp, %error, "terminal direct: terminal_retirement_failed");
        }
    }

    /// Drop a revoked key's grants everywhere, as a device and as a worker
    /// (v2 `main.ts` `closeRevokedSockets`).
    pub fn release_revoked_key(&self, fingerprint: &str) {
        self.retire_worker(fingerprint, TerminalDirectRetireReason::WorkerRevoked);
        if let Err(error) = self.grants.revoke_device(fingerprint) {
            tracing::warn!(fingerprint, %error, "terminal direct: device_revocation_failed");
        }
    }
}
