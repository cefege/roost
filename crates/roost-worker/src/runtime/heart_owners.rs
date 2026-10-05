//! The W-HEART owners, built once over the session stack: the folder-facts
//! watcher (attached to the manager's folder and closed hooks), the stray
//! sweeper and the keeper reconciliation stamp (both handed to the reconcile
//! pass), and the heartbeat loop started after readiness. Composes v2
//! `apps/worker/src/main.ts:309-325` (`startHeartbeat`) and `:342` (stop).
//! Built by `runtime::owners::WorkerOwners::build`; stopped by its `shutdown`.

use std::sync::Arc;

use roost_host::{HostPlatform, ProcessEnv};
use roost_observability::clock::EventClock;

use super::WorkerBoot;
use super::bootstrap_redeem::{activation, build_sha};
use super::credential::WorkerKeyCredential;
use super::heartbeat::{CapacityReader, HeartbeatConfig, HeartbeatHandle, KeeperReconciliation};
use super::heartbeat_metrics::HostMetricsCollector;
use super::heartbeat_sources::{CoordinatorHeartbeatRpc, WorkerHeartbeatSources};
use super::reachable_addr::ReachableAddr;
use super::session_stack::SessionStack;
use crate::host::identity::static_host_identity;
use crate::host::tailnet::tailscale_binary_candidates;
use crate::keeper_pool::KeeperPool;
use crate::session::git_ports::SessionFolderFacts;
use crate::session::lifecycle::SessionManager;
use crate::session::stray_reap::StraySweeper;

/// Every W-HEART owner, for the life of the link.
pub struct HeartOwners {
    /// v2 `reapStrayKeeperChannels` + its timer; the reconcile pass drives it.
    pub strays: Arc<StraySweeper>,
    /// v2 `keeperReconciledAtMs`; the reconcile pass writes, the heartbeat reads.
    pub reconciliation: KeeperReconciliation,
    /// v2 `session-git-ports.ts`, keyed by session.
    pub folder_facts: Arc<SessionFolderFacts>,
    manager: Arc<SessionManager>,
    pool: Arc<KeeperPool>,
    clock: Arc<dyn EventClock>,
    platform: HostPlatform,
    heartbeat: Option<HeartbeatHandle>,
}

impl std::fmt::Debug for HeartOwners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeartOwners")
            .field("strays", &self.strays)
            .field("reconciled_at_ms", &self.reconciliation.current())
            .field("heartbeat_running", &self.heartbeat.is_some())
            .finish_non_exhaustive()
    }
}

impl HeartOwners {
    /// Build over `stack` and attach the folder watcher to its manager. Must
    /// run inside the worker's runtime and BEFORE any survivor is adopted, so
    /// an adopted session's folder is watched like a spawned one.
    pub fn build(stack: &SessionStack, pool: Arc<KeeperPool>, platform: HostPlatform) -> Self {
        let folder_facts =
            SessionFolderFacts::attach(&stack.manager, platform, tokio::runtime::Handle::current());
        tracing::info!(
            "the heart owners are built: folder facts, stray sweeper, reconciliation stamp"
        );
        Self {
            strays: StraySweeper::new(Arc::clone(&stack.manager)),
            reconciliation: KeeperReconciliation::default(),
            folder_facts,
            manager: Arc::clone(&stack.manager),
            pool,
            clock: stack.clock.clone(),
            platform,
            heartbeat: None,
        }
    }

    /// Start the heartbeat loop (v2 `startHeartbeat`, after readiness). The
    /// first beat is not awaited: see `heartbeat::spawn_heartbeat`.
    pub fn start_heartbeat(&mut self, boot: &WorkerBoot) -> anyhow::Result<()> {
        if self.heartbeat.is_some() {
            return Ok(());
        }
        let client = activation::coordinator_client(&boot.coordinator_base)?;
        let credential = Arc::new(WorkerKeyCredential::new(boot.worker_key_path.clone()));
        let env = ProcessEnv::new();
        let sources = WorkerHeartbeatSources::new(
            HostMetricsCollector::for_host(self.platform, Arc::clone(&self.clock)),
            build_sha(&env),
            ReachableAddr::from_tailnet(
                tailscale_binary_candidates(self.platform, &env),
                Arc::clone(&self.clock),
            ),
            Arc::clone(&self.pool),
        );
        let capacity = Arc::clone(self.manager.terminal_core_capacity());
        let read_capacity: CapacityReader = Arc::new(move || capacity.snapshot());
        let (handle, _first_settled) = super::heartbeat::spawn_heartbeat(HeartbeatConfig {
            rpc: Arc::new(CoordinatorHeartbeatRpc::new(client, credential)),
            reconciliation: self.reconciliation.clone(),
            read_terminal_core_capacity: Some(read_capacity),
            sources: Arc::new(sources),
            os: self.platform.as_str(),
            host_identity: static_host_identity(),
        });
        self.heartbeat = Some(handle);
        Ok(())
    }

    /// v2 shutdown: stop the heartbeat first, then the stray timer and every
    /// folder watcher. The keeper and its PTYs are untouched.
    pub fn shutdown(self) {
        if let Some(heartbeat) = &self.heartbeat {
            heartbeat.stop();
        }
        self.strays.dispose();
        let stopped = self.folder_facts.stop_all();
        tracing::info!(folder_watchers = stopped, "the heart owners were stopped");
    }
}
