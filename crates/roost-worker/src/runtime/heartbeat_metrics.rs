//! The host metrics a heartbeat ships: a real sample at most once a minute
//! (beats in between reuse it), bandwidth as the rate between the last two real
//! samples, and a log-only trail of this process's cgroup memory throttle.
//! Ports v2 `apps/worker/src/transport/heartbeat.ts` `collectHostMetrics` and
//! `logCgroupPressure`. `runtime::heartbeat_sources` owns one and calls it on a
//! blocking thread; the counters come from `host::samples`.

use std::sync::Arc;

use roost_host::HostPlatform;
use roost_observability::clock::EventClock;
use roost_protocol::wire::HostMetrics;

use crate::host::samples::{CgroupPressure, HostSampler, sample_cgroup_pressure};
use crate::host::{HostSample, NetCounters};

/// v2 `HOST_METRICS_INTERVAL_MS`: how long one real sample is reused.
pub const HOST_METRICS_INTERVAL_MS: i64 = 60_000;

/// v2 `CGROUP_RELOG_EVERY`: ~10 min of breadcrumbs at the 60 s sample cadence.
const CGROUP_RELOG_EVERY: u32 = 10;

/// One raw host sample (v2 `sampleHost`).
pub type HostSampleSource = Box<dyn FnMut() -> HostSample + Send>;
/// This process's cgroup throttle counters (v2 `cgroupPressure`).
pub type CgroupSource = Box<dyn FnMut() -> Option<CgroupPressure> + Send>;

/// The cached resampler and its bandwidth and cgroup baselines.
pub struct HostMetricsCollector {
    sample: HostSampleSource,
    cgroup: CgroupSource,
    clock: Arc<dyn EventClock>,
    cached: Option<HostMetrics>,
    previous_net: Option<(NetCounters, i64)>,
    cgroup_over_streak: u32,
    previous_high_events: Option<u64>,
}

impl std::fmt::Debug for HostMetricsCollector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostMetricsCollector")
            .field("cached", &self.cached)
            .finish_non_exhaustive()
    }
}

impl HostMetricsCollector {
    /// This host's collector: its platform's sampler, and the cgroup probe on
    /// Linux only (v2 picks both at module load).
    pub fn for_host(platform: HostPlatform, clock: Arc<dyn EventClock>) -> Self {
        let mut sampler = HostSampler::new(platform);
        let cgroup: CgroupSource = match platform {
            HostPlatform::Linux => Box::new(sample_cgroup_pressure),
            HostPlatform::MacOs | HostPlatform::Windows => Box::new(|| None),
        };
        Self::new(Box::new(move || sampler.sample()), cgroup, clock)
    }

    pub fn new(sample: HostSampleSource, cgroup: CgroupSource, clock: Arc<dyn EventClock>) -> Self {
        Self {
            sample,
            cgroup,
            clock,
            cached: None,
            previous_net: None,
            cgroup_over_streak: 0,
            previous_high_events: None,
        }
    }

    /// The metrics for this beat. Blocking: a real sample runs host tools.
    pub fn collect(&mut self) -> HostMetrics {
        // The cache gate leaves the bandwidth baseline untouched, so the next
        // real sample's delta still spans the whole minute.
        if let Some(cached) = &self.cached
            && self.clock.now_epoch_ms() - cached.sampled_at_ms < HOST_METRICS_INTERVAL_MS
        {
            return cached.clone();
        }
        // After the cache gate: one probe per real sample, which is what
        // `CGROUP_RELOG_EVERY` is calibrated against.
        self.log_cgroup_pressure();
        let sampled_at_ms = self.clock.now_epoch_ms();
        let sample = (self.sample)();
        let (mut net_rx_bps, mut net_tx_bps) = (0, 0);
        if let (Some(net), Some((previous, previous_at_ms))) = (sample.net, self.previous_net) {
            let seconds = (sampled_at_ms - previous_at_ms) as f64 / 1000.0;
            if seconds > 0.0 {
                // A counter that wrapped or reset (interface down/up) reads as
                // zero for this sample, never as a negative or a wrap-sized rate.
                net_rx_bps = rate(net.rx_bytes.checked_sub(previous.rx_bytes), seconds);
                net_tx_bps = rate(net.tx_bytes.checked_sub(previous.tx_bytes), seconds);
            }
        }
        if let Some(net) = sample.net {
            self.previous_net = Some((net, sampled_at_ms));
        }
        let metrics = HostMetrics {
            cpu_pct: sample.cpu_pct,
            mem_used_bytes: signed(sample.mem_used_bytes),
            mem_total_bytes: signed(sample.mem_total_bytes),
            disk_used_bytes: signed(sample.disk_used_bytes),
            disk_total_bytes: signed(sample.disk_total_bytes),
            net_rx_bps,
            net_tx_bps,
            sampled_at_ms,
        };
        tracing::debug!(?metrics, "host metrics were sampled");
        self.cached = Some(metrics.clone());
        metrics
    }

    /// v2 `logCgroupPressure`: log-only, a line when the unit first goes over
    /// its `memory.high`, every tenth sample while it stays over, and one when
    /// it clears.
    fn log_cgroup_pressure(&mut self) {
        let Some(pressure) = (self.cgroup)() else {
            return;
        };
        let high_events_delta = self
            .previous_high_events
            .map_or(0, |previous| pressure.high_events.saturating_sub(previous));
        self.previous_high_events = Some(pressure.high_events);
        if pressure.current_bytes > pressure.high_bytes {
            let first = self.cgroup_over_streak == 0;
            self.cgroup_over_streak += 1;
            if first || self.cgroup_over_streak.is_multiple_of(CGROUP_RELOG_EVERY) {
                tracing::warn!(
                    current_bytes = pressure.current_bytes,
                    high_bytes = pressure.high_bytes,
                    high_events_delta,
                    "cgroup_memory_high_exceeded"
                );
            }
        } else if self.cgroup_over_streak > 0 {
            tracing::info!(
                current_bytes = pressure.current_bytes,
                high_bytes = pressure.high_bytes,
                "cgroup_memory_high_cleared"
            );
            self.cgroup_over_streak = 0;
        }
    }
}

/// Bytes per second over `seconds`, rounded as v2's `Math.round`.
fn rate(delta: Option<u64>, seconds: f64) -> i64 {
    delta.map_or(0, |bytes| (bytes as f64 / seconds).round() as i64)
}

fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
