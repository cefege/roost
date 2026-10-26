//! The loop behind [`super::LiveMetricsOwner`]: one sample per tick, taken off the
//! async runtime, published fenced to the connection that was current before the
//! sample began. The loop owns its sampler, so cancelling it drops the sampler.

use std::sync::Arc;
use std::time::Duration;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_protocol::wire::live_metrics::{LiveHostSnapshot, LiveProcess};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::host::live::{LiveSampler, LiveSnapshot};
use crate::uplink::{LinkFence, Uplink};

/// Reads host figures. Object-safe so the owner can hold a factory of them.
pub(super) trait SampleSource: Send + 'static {
    fn sample(&mut self) -> LiveSnapshot;
}

impl SampleSource for LiveSampler {
    fn sample(&mut self) -> LiveSnapshot {
        LiveSampler::sample(self)
    }
}

/// Where samples go. `fence` is read before a sample is taken, so a connection
/// that changes during the read refuses the sample instead of receiving it.
pub(super) trait SampleSink: Send + Sync {
    fn fence(&self) -> LinkFence;
    fn publish(&self, fence: &LinkFence, snapshot: LiveHostSnapshot) -> bool;
}

impl SampleSink for Uplink {
    fn fence(&self) -> LinkFence {
        Uplink::fence(self)
    }

    fn publish(&self, fence: &LinkFence, snapshot: LiveHostSnapshot) -> bool {
        self.send_fenced(fence, CoordWorkerUpstream::LiveMetrics(snapshot))
    }
}

/// Samples every `interval` until `cancel` fires. Cancellation is checked before
/// the tick and again after the sample, so a stop never publishes a sample taken
/// after it was requested.
pub(super) async fn run_sampling(
    mut sampler: Box<dyn SampleSource>,
    sink: Arc<dyn SampleSink>,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let fence = sink.fence();
        // The sampler moves into the blocking task and comes back with its reading.
        // A join failure means the sampler is gone, so the loop ends with it.
        let Ok((returned, snapshot)) = tokio::task::spawn_blocking(move || {
            let snapshot = sampler.sample();
            (sampler, snapshot)
        })
        .await
        else {
            return;
        };
        sampler = returned;
        if cancel.is_cancelled() {
            return;
        }
        match to_wire(snapshot) {
            Some(wire) => {
                sink.publish(&fence, wire);
            }
            None => tracing::warn!("a live host sample broke the wire rules and was dropped"),
        }
    }
}

/// The wire form of a sample, or `None` when the wire rules refuse it. The
/// sampler bounds every field, so a refusal is a sampler defect.
fn to_wire(snapshot: LiveSnapshot) -> Option<LiveHostSnapshot> {
    let wire = LiveHostSnapshot {
        cpu_pct: snapshot.cpu_pct,
        mem_used_bytes: snapshot.mem_used_bytes,
        mem_total_bytes: snapshot.mem_total_bytes,
        disk_used_bytes: snapshot.disk_used_bytes,
        disk_total_bytes: snapshot.disk_total_bytes,
        cpu_count: snapshot.cpu_count,
        uptime_secs: snapshot.uptime_secs,
        process_count: snapshot.process_count,
        processes: snapshot
            .processes
            .into_iter()
            .map(|process| LiveProcess {
                pid: process.pid,
                ppid: process.ppid,
                name: process.name,
                cpu_pct: process.cpu_pct,
                rss_bytes: process.rss_bytes,
            })
            .collect(),
        sampled_at_ms: snapshot.sampled_at_ms,
    };
    wire.check().is_ok().then_some(wire)
}
