//! Worker-side owner of live host metrics. A coordinator subscription starts or
//! stops one sampling task; the link lifecycle stops it on every open and detach,
//! so each new link starts off until the coordinator subscribes again. Samples
//! are latest-only and never persisted.

mod sampling;
#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use roost_proto::DLiveMetricsSubscription;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::host::live::LiveSampler;
use crate::link_ports::{LinkLifecyclePort, LiveMetricsPort};
use crate::uplink::Uplink;

use sampling::{SampleSink, SampleSource, run_sampling};

/// How often a subscribed coordinator receives a sample.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

type SamplerFactory = Arc<dyn Fn() -> Box<dyn SampleSource> + Send + Sync>;

struct RunningSampling {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

/// Starts and stops the sampling task. Holds at most one running task.
pub struct LiveMetricsOwner {
    worker_id: String,
    sink: Arc<dyn SampleSink>,
    make_sampler: SamplerFactory,
    interval: Duration,
    running: Mutex<Option<RunningSampling>>,
}

impl LiveMetricsOwner {
    /// Samples the host and publishes each sample on `uplink`. A subscription
    /// spawns the sampling task, so this must be called inside the tokio runtime
    /// that drives the worker.
    pub fn new(worker_id: impl Into<String>, uplink: Uplink) -> Self {
        Self::with_parts(
            worker_id.into(),
            Arc::new(uplink),
            Arc::new(|| Box::new(LiveSampler::new()) as Box<dyn SampleSource>),
            SAMPLE_INTERVAL,
        )
    }

    fn with_parts(
        worker_id: String,
        sink: Arc<dyn SampleSink>,
        make_sampler: SamplerFactory,
        interval: Duration,
    ) -> Self {
        Self {
            worker_id,
            sink,
            make_sampler,
            interval,
            running: Mutex::new(None),
        }
    }

    /// Starts sampling unless it is already running. Each start builds a fresh
    /// sampler, so a stopped subscription releases its sysinfo state.
    pub fn start(&self) {
        let mut running = self.lock_running();
        if running
            .as_ref()
            .is_some_and(|sampling| !sampling.task.is_finished())
        {
            return;
        }
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_sampling(
            (self.make_sampler)(),
            Arc::clone(&self.sink),
            self.interval,
            cancel.clone(),
        ));
        *running = Some(RunningSampling { cancel, task });
        tracing::info!(worker_id = %self.worker_id, "live metrics sampling started");
    }

    /// Stops sampling. The task ends at its next wake-up and drops its sampler
    /// then. Stopping when nothing runs does nothing.
    pub fn stop(&self) {
        let Some(sampling) = self.lock_running().take() else {
            return;
        };
        sampling.cancel.cancel();
        if !sampling.task.is_finished() {
            tracing::info!(worker_id = %self.worker_id, "live metrics sampling stopped");
        }
    }

    fn lock_running(&self) -> MutexGuard<'_, Option<RunningSampling>> {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for LiveMetricsOwner {
    fn drop(&mut self) {
        self.stop();
    }
}

impl fmt::Debug for LiveMetricsOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveMetricsOwner")
            .field("worker_id", &self.worker_id)
            .finish_non_exhaustive()
    }
}

impl LiveMetricsPort for LiveMetricsOwner {
    fn subscription(&self, request: DLiveMetricsSubscription) {
        if request.active {
            self.start();
        } else {
            self.stop();
        }
    }
}

/// Turns sampling off on every link change. A coordinator re-subscribes after it
/// reconnects, and a sample taken with no link attached has nowhere to go.
#[derive(Debug)]
pub struct LiveMetricsLinkLifecycle(pub Arc<LiveMetricsOwner>);

impl LinkLifecyclePort for LiveMetricsLinkLifecycle {
    fn on_open(&self) {
        self.0.stop();
    }
    fn on_hello_ack(&self, _capabilities: &[String]) {}
    fn on_detach(&self) {
        self.0.stop();
    }
    fn on_writable(&self) {}
    fn on_snapshot_ready(&self) {}
}
