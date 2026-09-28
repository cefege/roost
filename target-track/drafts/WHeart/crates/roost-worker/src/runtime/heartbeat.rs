//! The completion-scheduled heartbeat: one bounded `WorkersHeartbeat` RPC,
//! then the next one 30 s after it settles, so sampling and RPCs never
//! overlap. Each beat carries host metrics (the last good sample when this one
//! failed), git sha, os, host identity, the keeper runtime proof (only after a
//! reconciliation it still belongs to), the reachable address and the
//! terminal-core capacity. Ports v2 `apps/worker/src/transport/heartbeat.ts`
//! (`startHeartbeat`); `runtime::heart_owners` starts it with the production
//! sources of `runtime::heartbeat_sources`.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use roost_observability::fields::LogFields;
use roost_observability::signal;
use roost_observability::signal_kind::SignalKind;
use roost_proto::WorkersHeartbeatRequest;
use roost_proto::buffa::MessageField;
use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::proto_adapters::{
    host_identity_to_proto, keeper_runtime_observation_to_proto,
    terminal_core_capacity_report_to_proto,
};
use roost_protocol::wire::{HostIdentity, HostMetrics, TerminalCoreCapacityReport};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::uplink::OwnerFuture;

/// v2 `HEARTBEAT_INTERVAL_MS`: from one attempt's settlement to the next start.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// v2 `HEARTBEAT_RPC_TIMEOUT_MS`: the deadline every attempt's RPC carries.
pub const HEARTBEAT_RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// Consecutive failed attempts, within one loop, before `heartbeat.stalled`.
const HEARTBEAT_STALL_AFTER: u32 = 3;

/// What a beat reads besides the terminal-core capacity (v2 `HeartbeatSources`).
pub trait HeartbeatSources: Send + Sync {
    /// Host metrics; `Err` keeps the last good sample (sampling is metadata).
    fn collect_host_metrics(&self) -> OwnerFuture<Result<HostMetrics, String>>;
    fn git_sha(&self) -> Option<String>;
    /// The authenticated keeper proof for the reconciliation stamped `ms`.
    fn observe_keeper_runtime(
        &self,
        reconciled_at_ms: i64,
    ) -> OwnerFuture<Result<Option<KeeperRuntimeObservationV1>, String>>;
    fn reachable_addr(&self) -> OwnerFuture<Option<String>>;
}

/// The coordinator call itself (v2 `client().workersHeartbeat(.., { timeoutMs })`).
pub trait HeartbeatRpc: Send + Sync {
    fn workers_heartbeat(
        &self,
        request: WorkersHeartbeatRequest,
        timeout: Duration,
    ) -> OwnerFuture<Result<(), String>>;
}

/// v2 `readTerminalCoreCapacity`: the worker-owned admission report.
pub type CapacityReader = Arc<dyn Fn() -> TerminalCoreCapacityReport + Send + Sync>;

/// v2 `main.ts` `keeperReconciledAtMs`: when the reconciliation the keeper
/// proof belongs to succeeded, `None` while one runs or none has. The
/// reconcile pass writes it (`started`, `reconciled`); the heartbeat reads it.
#[derive(Debug, Clone, Default)]
pub struct KeeperReconciliation {
    at_ms: Arc<Mutex<Option<i64>>>,
}

impl KeeperReconciliation {
    /// v2 `onReconcileStarted`: withhold the proof until this one succeeds.
    pub fn started(&self) {
        *self.at_ms.lock().unwrap_or_else(PoisonError::into_inner) = None;
        tracing::info!("keeper runtime proof withheld: a reconciliation started");
    }

    /// v2 `onReconciled`.
    pub fn reconciled(&self, at_ms: i64) {
        *self.at_ms.lock().unwrap_or_else(PoisonError::into_inner) = Some(at_ms);
        tracing::info!(at_ms, "keeper runtime proof admitted: a reconciliation succeeded");
    }

    pub fn current(&self) -> Option<i64> {
        *self.at_ms.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Everything one heartbeat loop runs with.
pub struct HeartbeatConfig {
    pub rpc: Arc<dyn HeartbeatRpc>,
    pub reconciliation: KeeperReconciliation,
    pub read_terminal_core_capacity: Option<CapacityReader>,
    pub sources: Arc<dyn HeartbeatSources>,
    /// v2 `HOST_PLATFORM`, re-sent every beat.
    pub os: &'static str,
    /// v2 `staticHostIdentity()`, read once when the loop starts.
    pub host_identity: Option<HostIdentity>,
}

/// v2 `HeartbeatDisposer`: stops scheduling; an attempt in flight completes.
/// Dropping the handle stops the loop too, so a loop nothing can stop does not
/// outlive whoever started it.
#[derive(Debug)]
pub struct HeartbeatHandle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl HeartbeatHandle {
    /// Stop the loop. Idempotent. An attempt already in flight is not
    /// cancelled; no later attempt is scheduled.
    pub fn stop(&self) {
        if !self.stop.send_replace(true) {
            tracing::info!("the heartbeat loop was stopped");
        }
    }

    /// Whether the loop task has ended.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// Start the loop and return once its FIRST attempt has settled (v2
/// `await startHeartbeat(..)`).
pub async fn start_heartbeat(config: HeartbeatConfig) -> HeartbeatHandle {
    let (handle, first_settled) = spawn_heartbeat(config);
    if first_settled.await.is_err() {
        tracing::warn!("the heartbeat loop ended before its first attempt settled");
    }
    handle
}

/// Start the loop without waiting for its first attempt. The composition root
/// uses this: v2's link was already dialling when it awaited the first beat,
/// and awaiting it here, before the link runs, would delay the dial by up to
/// the RPC deadline against an unreachable coordinator.
pub fn spawn_heartbeat(config: HeartbeatConfig) -> (HeartbeatHandle, oneshot::Receiver<()>) {
    let (stop, mut stopped) = watch::channel(false);
    let (settled, first_settled) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut beat = Beat {
            config,
            consecutive_misses: 0,
            last_good_host_metrics: None,
        };
        beat.attempt().await;
        // v2 `scheduleNext` runs before `startHeartbeat` resolves: the next
        // attempt's deadline is fixed at settlement, not when a waiter wakes.
        let mut next = Box::pin(tokio::time::sleep(HEARTBEAT_INTERVAL));
        let _ = settled.send(());
        loop {
            tokio::select! {
                biased;
                changed = stopped.changed() => {
                    if changed.is_err() || *stopped.borrow() {
                        break;
                    }
                }
                () = &mut next => {
                    beat.attempt().await;
                    next.set(tokio::time::sleep(HEARTBEAT_INTERVAL));
                }
            }
        }
        tracing::info!("the heartbeat loop ended");
    });
    tracing::info!(interval_s = HEARTBEAT_INTERVAL.as_secs(), "the heartbeat loop started");
    (HeartbeatHandle { stop, task }, first_settled)
}

/// One loop instance's state: misses belong to it alone.
struct Beat {
    config: HeartbeatConfig,
    consecutive_misses: u32,
    last_good_host_metrics: Option<HostMetrics>,
}

impl Beat {
    /// v2 `beat`: sample, observe, send, and count the outcome.
    async fn attempt(&mut self) {
        let mut host_metrics = self.last_good_host_metrics.clone();
        match self.config.sources.collect_host_metrics().await {
            Ok(metrics) => {
                host_metrics = Some(metrics.clone());
                self.last_good_host_metrics = Some(metrics);
            }
            // Sampling is metadata, not liveness: keep the last complete
            // sample (or none before the first) and still contact coord.
            Err(error) => tracing::warn!(%error, "host metrics sample failed"),
        }
        let capacity = self
            .config
            .read_terminal_core_capacity
            .as_ref()
            .map(|read| read());
        match self.send(host_metrics, capacity).await {
            Ok(reachable_addr) => {
                tracing::debug!(?reachable_addr, "beat sent");
                self.consecutive_misses = 0;
            }
            Err(error) => {
                self.consecutive_misses += 1;
                tracing::warn!(%error, misses = self.consecutive_misses, "beat failed");
                if self.consecutive_misses >= HEARTBEAT_STALL_AFTER {
                    signal::emit(
                        SignalKind::HeartbeatStalled,
                        LogFields::new()
                            .set("misses", self.consecutive_misses)
                            .set("cooldownKey", "heartbeat"),
                    );
                }
            }
        }
    }

    /// Everything inside v2's `try`: a failure anywhere here is one miss.
    async fn send(
        &self,
        host_metrics: Option<HostMetrics>,
        capacity: Option<TerminalCoreCapacityReport>,
    ) -> Result<Option<String>, String> {
        let sources = &self.config.sources;
        let git_sha = sources.git_sha();
        let reconciliation = self.config.reconciliation.current();
        let mut keeper_runtime = None;
        if let Some(reconciled_at_ms) = reconciliation {
            match sources.observe_keeper_runtime(reconciled_at_ms).await {
                Ok(observation) => keeper_runtime = observation,
                Err(error) => tracing::warn!(%error, "keeper_runtime_observation_failed"),
            }
        }
        // A reconciliation that started or finished while the keeper was
        // probed makes the proof describe a state that no longer holds.
        if self.config.reconciliation.current() != reconciliation {
            keeper_runtime = None;
        }
        let reachable_addr = sources.reachable_addr().await;
        let request = WorkersHeartbeatRequest {
            host_metrics: host_metrics.as_ref().map_or_else(MessageField::none, |metrics| {
                MessageField::some(host_metrics_to_proto(metrics))
            }),
            git_sha,
            os: Some(self.config.os.to_owned()),
            host_identity: MessageField::some(host_identity_to_proto(
                self.config.host_identity.as_ref(),
            )),
            keeper_runtime: keeper_runtime
                .as_ref()
                .map(keeper_runtime_observation_to_proto)
                .transpose()
                .map_err(|error| error.to_string())?
                .map_or_else(MessageField::none, MessageField::some),
            reachable_addr: reachable_addr.clone(),
            terminal_core_capacity: capacity
                .as_ref()
                .map(terminal_core_capacity_report_to_proto)
                .transpose()
                .map_err(|error| error.to_string())?
                .map_or_else(MessageField::none, MessageField::some),
            ..Default::default()
        };
        self.config
            .rpc
            .workers_heartbeat(request, HEARTBEAT_RPC_TIMEOUT)
            .await?;
        Ok(reachable_addr)
    }
}

/// The wire metrics as the proto carries them (v2's inline `BigInt(..)`).
fn host_metrics_to_proto(metrics: &HostMetrics) -> roost_proto::HostMetrics {
    let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
    roost_proto::HostMetrics {
        cpu_pct: metrics.cpu_pct,
        mem_used_bytes: unsigned(metrics.mem_used_bytes),
        mem_total_bytes: unsigned(metrics.mem_total_bytes),
        disk_used_bytes: unsigned(metrics.disk_used_bytes),
        disk_total_bytes: unsigned(metrics.disk_total_bytes),
        net_rx_bps: unsigned(metrics.net_rx_bps),
        net_tx_bps: unsigned(metrics.net_tx_bps),
        sampled_at_ms: unsigned(metrics.sampled_at_ms),
        ..Default::default()
    }
}
