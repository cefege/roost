//! State-transition tests for the live metrics owner. Each start, stop, link change
//! and subscription is observed through what the sampling task does: how many
//! samplers were built and dropped, and what reached the sink.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use roost_proto::DLiveMetricsSubscription;
use roost_protocol::wire::live_metrics::LiveHostSnapshot;

use super::sampling::{SampleSink, SampleSource};
use super::{LiveMetricsLinkLifecycle, LiveMetricsOwner, SamplerFactory};
use crate::host::live::{LiveProcessSample, LiveSnapshot};
use crate::link_ports::{LinkLifecyclePort, LiveMetricsPort};
use crate::uplink::{LinkFence, Uplink};

const TICK: Duration = Duration::from_millis(2);

#[derive(Default)]
struct Probe {
    built: AtomicUsize,
    dropped: AtomicUsize,
    sampled: AtomicUsize,
    published: Mutex<Vec<LiveHostSnapshot>>,
}

impl Probe {
    fn built(&self) -> usize {
        self.built.load(Ordering::SeqCst)
    }

    fn dropped(&self) -> usize {
        self.dropped.load(Ordering::SeqCst)
    }

    fn sampled(&self) -> usize {
        self.sampled.load(Ordering::SeqCst)
    }

    fn published(&self) -> MutexGuard<'_, Vec<LiveHostSnapshot>> {
        self.published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn published_count(&self) -> usize {
        self.published().len()
    }
}

struct FakeSource {
    cpu_pct: f64,
    probe: Arc<Probe>,
}

impl SampleSource for FakeSource {
    fn sample(&mut self) -> LiveSnapshot {
        self.probe.sampled.fetch_add(1, Ordering::SeqCst);
        LiveSnapshot {
            cpu_pct: self.cpu_pct,
            mem_used_bytes: 1,
            mem_total_bytes: 2,
            disk_used_bytes: 1,
            disk_total_bytes: 2,
            cpu_count: 4,
            uptime_secs: 10,
            process_count: 1,
            processes: vec![LiveProcessSample {
                pid: 7,
                ppid: 1,
                name: "sshd".to_owned(),
                cpu_pct: 0.5,
                rss_bytes: 4096,
            }],
            sampled_at_ms: 1_700_000_000_000,
        }
    }
}

impl Drop for FakeSource {
    fn drop(&mut self) {
        self.probe.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct FakeSink {
    uplink: Uplink,
    probe: Arc<Probe>,
}

impl SampleSink for FakeSink {
    fn fence(&self) -> LinkFence {
        self.uplink.fence()
    }

    fn publish(&self, _fence: &LinkFence, snapshot: LiveHostSnapshot) -> bool {
        self.probe.published().push(snapshot);
        true
    }
}

fn owner_with(cpu_pct: f64) -> (LiveMetricsOwner, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    let factory_probe = Arc::clone(&probe);
    let make_sampler: SamplerFactory = Arc::new(move || -> Box<dyn SampleSource> {
        factory_probe.built.fetch_add(1, Ordering::SeqCst);
        Box::new(FakeSource {
            cpu_pct,
            probe: Arc::clone(&factory_probe),
        })
    });
    let sink = Arc::new(FakeSink {
        uplink: Uplink::detached(),
        probe: Arc::clone(&probe),
    });
    let owner =
        LiveMetricsOwner::with_parts("worker-a".to_owned(), sink, make_sampler, TICK);
    (owner, probe)
}

async fn settle_until(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the condition settles within five seconds");
}

#[tokio::test]
async fn start_publishes_samples_until_stopped() {
    let (owner, probe) = owner_with(37.5);
    owner.start();
    settle_until(|| probe.published_count() >= 2).await;

    owner.stop();
    settle_until(|| probe.dropped() == probe.built()).await;
    let settled = probe.published_count();
    tokio::time::sleep(TICK * 10).await;
    assert_eq!(
        probe.published_count(),
        settled,
        "no sample is published once the sampler is dropped"
    );
}

#[tokio::test]
async fn a_published_sample_carries_the_sampled_figures() {
    let (owner, probe) = owner_with(37.5);
    owner.start();
    settle_until(|| probe.published_count() >= 1).await;
    owner.stop();

    let first = probe.published()[0].clone();
    assert_eq!(first.cpu_pct, 37.5);
    assert_eq!(first.cpu_count, 4);
    assert_eq!(first.processes.len(), 1);
    assert_eq!(first.processes[0].name, "sshd");
    assert!(first.check().is_ok(), "a published sample passes the wire rules");
}

#[tokio::test]
async fn starting_a_running_owner_builds_one_sampler() {
    let (owner, probe) = owner_with(1.0);
    owner.start();
    owner.start();
    assert_eq!(probe.built(), 1, "a second start while running keeps the sampler");
    settle_until(|| probe.published_count() >= 1).await;
    owner.stop();
}

#[tokio::test]
async fn stopping_an_idle_owner_is_a_no_op() {
    let (owner, probe) = owner_with(1.0);
    owner.stop();
    owner.stop();
    assert_eq!(probe.built(), 0);
    assert_eq!(probe.dropped(), 0);
}

#[tokio::test]
async fn restart_after_stop_builds_a_new_sampler_and_samples_again() {
    let (owner, probe) = owner_with(1.0);
    owner.start();
    settle_until(|| probe.published_count() >= 1).await;
    owner.stop();
    settle_until(|| probe.dropped() == 1).await;

    let before = probe.published_count();
    owner.start();
    settle_until(|| probe.published_count() > before).await;
    assert_eq!(probe.built(), 2);
    owner.stop();
}

#[tokio::test]
async fn link_open_and_detach_each_turn_sampling_off() {
    let (owner, probe) = owner_with(1.0);
    let owner = Arc::new(owner);
    let lifecycle = LiveMetricsLinkLifecycle(Arc::clone(&owner));

    owner.start();
    settle_until(|| probe.published_count() >= 1).await;
    lifecycle.on_open();
    settle_until(|| probe.dropped() == 1).await;

    owner.start();
    lifecycle.on_detach();
    settle_until(|| probe.dropped() == 2).await;
    assert_eq!(probe.built(), 2);
}

#[tokio::test]
async fn subscription_follows_the_active_flag() {
    let (owner, probe) = owner_with(1.0);
    owner.subscription(DLiveMetricsSubscription {
        active: true,
        ..Default::default()
    });
    assert_eq!(probe.built(), 1);
    settle_until(|| probe.published_count() >= 1).await;

    owner.subscription(DLiveMetricsSubscription {
        active: false,
        ..Default::default()
    });
    settle_until(|| probe.dropped() == 1).await;
}

#[tokio::test]
async fn a_sample_the_wire_rules_refuse_is_never_published() {
    let (owner, probe) = owner_with(150.0);
    owner.start();
    settle_until(|| probe.sampled() >= 3).await;
    owner.stop();
    assert_eq!(probe.published_count(), 0);
}
