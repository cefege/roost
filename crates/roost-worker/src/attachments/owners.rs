//! The attachment owners one worker process runs: the operation owner every
//! carrier writes through with its 60 s idle sweep, the reaper's boot and
//! hourly sweep, the direct-grant store and the direct carriers built on them.
//! Ports the composition in v2 `apps/worker/src/main.ts:196-199`,
//! `attachments/attachment-upload.ts:40-43` and `boot/boot-local-terminal.ts`.
//! Started by `runtime::owners`.

use std::sync::Arc;

use tokio::task::JoinHandle;

use super::direct_owners::{AttachmentDirect, AttachmentDirectDeps};
use super::grants::AttachmentGrantStore;
use super::link::AttachmentLink;
use super::reaper::start_attachment_reaper;
use super::store_paths::AttachmentBase;
use super::system_clock;
use super::upload::AttachmentOperations;
use crate::link_ports::AttachmentLinkPort;
use crate::peer::CoordinatorGeneration;
use crate::peer::PeerTransportConfig;
use crate::peer::native::NativeLoader;

/// What the attachment owners are built from.
pub struct AttachmentOwnersDeps {
    pub base: AttachmentBase,
    pub process_epoch: String,
    pub worker_fingerprint: String,
    pub peer: PeerTransportConfig,
    /// The ONE native peer load the terminal peer owner also uses.
    pub native_loader: NativeLoader,
    /// The generation gate the terminal peer owner shares.
    pub coordinator_generation: CoordinatorGeneration,
}

impl std::fmt::Debug for AttachmentOwnersDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentOwnersDeps")
            .field("base", &self.base)
            .field("process_epoch", &self.process_epoch)
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

/// The shared attachment state, the direct carriers over it, and the two
/// periodic tasks that bound it.
#[derive(Debug)]
pub struct AttachmentOwners {
    /// The one operation owner: the relay arm, the loopback socket and the
    /// attachment peer all write through clones of it.
    pub operations: AttachmentOperations,
    /// The grants the coordinator installs for direct carriers.
    pub grants: Arc<AttachmentGrantStore>,
    /// The loopback sockets and the attachment peer owner.
    pub direct: AttachmentDirect,
    idle_sweep: JoinHandle<()>,
    reaper: JoinHandle<()>,
}

impl AttachmentOwners {
    /// Build the owners and start both sweeps. Must run inside the worker's
    /// tokio runtime.
    pub fn start(deps: AttachmentOwnersDeps) -> Self {
        let operations = AttachmentOperations::new(deps.base.clone(), system_clock());
        let idle_sweep = operations.spawn_idle_sweep();
        let reaper = start_attachment_reaper(deps.base);
        let grants = Arc::new(AttachmentGrantStore::system(deps.process_epoch.clone()));
        let direct = AttachmentDirect::new(AttachmentDirectDeps {
            grants: Arc::clone(&grants),
            operations: operations.clone(),
            worker_fingerprint: deps.worker_fingerprint,
            worker_epoch: deps.process_epoch,
            peer: deps.peer,
            native_loader: deps.native_loader,
            coordinator_generation: deps.coordinator_generation,
        });
        tracing::info!("the attachment owners started: operation idle sweep and reaper running");
        Self {
            operations,
            grants,
            direct,
            idle_sweep,
            reaper,
        }
    }

    /// The coordinator link's attachment port over these owners.
    pub fn link(&self) -> Arc<dyn AttachmentLinkPort> {
        Arc::new(AttachmentLink::new(
            self.operations.clone(),
            Arc::clone(&self.grants),
            self.direct.clone(),
        ))
    }

    /// Stop both sweeps. The grants are disposed by the direct carriers' own
    /// dispose, after their sockets (v2 `disposeDirect` order).
    pub fn shutdown(&self) {
        self.idle_sweep.abort();
        self.reaper.abort();
        tracing::info!("the attachment idle sweep and reaper were stopped");
    }
}
