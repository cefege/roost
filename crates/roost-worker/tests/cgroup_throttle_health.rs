//! The third layer of "a worker throttled by its own cgroup looks healthy": the
//! trail that says the throttle is happening at all.
//!
//! The first two layers are the systemd `MemoryHigh` that scales with the host
//! and the reconnect ladder's `has_opened`; `tests/backoff_policy.rs` and
//! `tests/worker_reconnect_ladder.rs` pin those. This file pins the third,
//! which is the only one that makes the next occurrence a grep instead of a
//! guess: `/proc/meminfo` is host-wide, so a unit sitting above its own
//! `memory.high` publishes "8.7 GB of 33.6 GB used" from inside a cgroup where
//! every allocation is being throttled, and nothing in that number says so.
//!
//! The observable is the log line, because the log line is what an operator
//! reads. There is no API here that a heartbeat or a dashboard consumes, so
//! asserting the event is asserting the product, not a seam.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use roost_observability::clock::EventClock;
use roost_worker::host::samples::CgroupPressure;
use roost_worker::host::{HostSample, NetCounters};
use roost_worker::runtime::heartbeat_metrics::{HOST_METRICS_INTERVAL_MS, HostMetricsCollector};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// The message v2 `logCgroupPressure` emits and `docs/FAILURE-INDEX.md` tells
/// an operator to grep for. Spelled out rather than imported because it is the
/// contract with whoever reads the log, not an internal detail.
const EXCEEDED: &str = "cgroup_memory_high_exceeded";
const CLEARED: &str = "cgroup_memory_high_cleared";

/// `v2 CGROUP_RELOG_EVERY`, which the collector keeps private. The test needs it
/// to drive the cadence, and a cadence the test cannot name is a cadence it
/// cannot pin.
const RELOG_EVERY: usize = 10;

/// A clock the test steps by hand, so a "one real sample per minute" claim is
/// proved by arithmetic instead of by sleeping.
#[derive(Debug, Default)]
struct StepClock(AtomicI64);

impl StepClock {
    fn set(&self, epoch_ms: i64) {
        self.0.store(epoch_ms, Ordering::SeqCst);
    }
}

impl EventClock for StepClock {
    fn now_epoch_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn mono_ns(&self) -> u64 {
        0
    }
}

/// One recorded event: level, message, and the fields an operator would read
/// off the line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    level: String,
    message: String,
    fields: Vec<(String, String)>,
}

impl Recorded {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A field visitor that keeps the event's own name/value pairs, so the
/// assertions are about the numbers on the line rather than about the call
/// that produced them.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl Visit for FieldVisitor {
    /// The event's `message` arrives HERE and not through `record_str`: the
    /// `tracing` macros record it as `format_args!`, whose `Value`
    /// implementation hands a visitor the arguments themselves and whose
    /// `Debug` is its `Display`, so `{:?}` is the bare message rather than a
    /// quoted one.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
            return;
        }
        self.fields
            .push((field.name().to_string(), format!("{value:?}")));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
            return;
        }
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
}

/// The thread-local recorder, the same shape `roast-observability`'s own tests
/// use: a layer rather than a global subscriber, so nothing here can be
/// observed by, or interfere with, a test running beside it.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Recorded>>>);

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let recorded = Recorded {
            level: event.metadata().level().to_string(),
            message: visitor.message.unwrap_or_default(),
            fields: visitor.fields,
        };
        if let Ok(mut events) = self.0.lock() {
            events.push(recorded);
        }
    }
}

/// Run `body` with the recorder current, and hand back what the throttle trail
/// said.
fn throttle_trail(body: impl FnOnce()) -> Vec<Recorded> {
    let capture = Capture::default();
    // `INFO` and up, so the per-sample DEBUG line the collector also emits does
    // not drown the trail. It cannot make a test vacuous: every assertion here
    // is about a trail that is NOT empty, so a line that dropped to DEBUG would
    // fail rather than pass.
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        // INSIDE the scope, and before the body: `tracing` decides once per
        // callsite whether anything is interested and caches it process-wide, so
        // a callsite another test already saw decline would leave this capture
        // silently empty. See `roast-observability`'s own capture for the same
        // note.
        tracing::callsite::rebuild_interest_cache();
        body();
    });
    let events = capture.0.lock().map(|events| events.clone());
    events.unwrap_or_default()
}

/// A throttle reading: how much this cgroup is using, how much it is allowed
/// before it starts being throttled, and how many times it has been.
fn pressure(current: u64, high: u64, events: u64) -> Option<CgroupPressure> {
    Some(CgroupPressure {
        current_bytes: current,
        high_bytes: high,
        high_events: events,
    })
}

/// A host sample with nothing in it, because nothing in this file is about the
/// host sample.
fn host_sample() -> HostSample {
    HostSample {
        cpu_pct: 0.0,
        mem_used_bytes: 0,
        mem_total_bytes: 0,
        disk_used_bytes: 0,
        disk_total_bytes: 0,
        net: Some(NetCounters::default()),
    }
}

/// A collector whose cgroup probe replays a script, and the number of times it
/// was asked. The count is an assertion in its own right: a probe that runs on
/// every heartbeat rather than on every real sample would turn the trail into a
/// per-beat flood and make `high_events_delta` unreadable.
fn collector(
    script: Vec<Option<CgroupPressure>>,
) -> (HostMetricsCollector, Arc<StepClock>, Arc<AtomicUsize>) {
    let clock = Arc::new(StepClock(AtomicI64::new(1_000_000)));
    let probes = Arc::new(AtomicUsize::new(0));
    let mut remaining = script.into_iter();
    let counted = Arc::clone(&probes);
    let collector = HostMetricsCollector::new(
        Box::new(host_sample),
        Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            remaining.next().flatten()
        }),
        Arc::clone(&clock) as Arc<dyn EventClock>,
    );
    (collector, clock, probes)
}

/// Step the clock past the host-metrics interval and take one real sample, which
/// is the only moment the throttle probe runs.
fn beat(collector: &mut HostMetricsCollector, clock: &StepClock, index: i64) {
    clock.set(1_000_000 + index * HOST_METRICS_INTERVAL_MS);
    collector.collect();
}

/// A unit sitting above its own `memory.high` says so, and a unit under it says
/// nothing. The second half is the control that makes the first mean something:
/// a probe that logged unconditionally would pass a test that only ever looked
/// for the line.
#[test]
fn a_unit_over_its_own_memory_high_says_so_and_a_unit_under_it_does_not() {
    let (mut collector, clock, probes) = collector(vec![
        // Under the limit: host-wide memory looks fine and nothing is wrong.
        pressure(2_000_000_000, 3_221_225_472, 0),
        // Over it, and the throttle counter has moved 150_000 times since the
        // last probe — the shape of the 2026-08-01 incident on ovh1.
        pressure(3_401_814_016, 3_221_225_472, 150_000),
    ]);

    let trail = throttle_trail(|| {
        beat(&mut collector, &clock, 1);
        beat(&mut collector, &clock, 2);
    });

    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert_eq!(
        trail.len(),
        1,
        "one line, for the one sample that was over its limit: {trail:?}"
    );
    assert_eq!(trail[0].message, EXCEEDED);
    assert_eq!(trail[0].level, "WARN");
    assert_eq!(
        trail[0].field("current_bytes"),
        Some("3401814016"),
        "the line has to carry the cgroup's own usage, because the host-wide \
         figure on the same beat is what sent the incident looking at the wrong \
         number"
    );
    assert_eq!(trail[0].field("high_bytes"), Some("3221225472"));
    assert_eq!(
        trail[0].field("high_events_delta"),
        Some("150000"),
        "a first reading has no previous one to subtract, so the delta is the \
         counter itself; it is the severity an operator reads"
    );
}

/// The trail follows the TRANSITION, not the level. A throttle that clears and
/// comes back is a second incident, and a collector that kept its streak across
/// the clearing would report the recurrence as a continuation of the first one
/// — or, worse, swallow it until the tenth sample.
#[test]
fn a_throttle_that_clears_and_returns_is_reported_as_a_second_incident() {
    let (mut collector, clock, _) = collector(vec![
        pressure(3_400_000_000, 3_000_000_000, 10),
        pressure(2_900_000_000, 3_000_000_000, 10),
        pressure(3_400_000_000, 3_000_000_000, 20),
    ]);

    let trail = throttle_trail(|| {
        beat(&mut collector, &clock, 1);
        beat(&mut collector, &clock, 2);
        beat(&mut collector, &clock, 3);
    });

    let messages: Vec<&str> = trail.iter().map(|event| event.message.as_str()).collect();
    assert_eq!(
        messages,
        vec![EXCEEDED, CLEARED, EXCEEDED],
        "over, clear, over again is three transitions and each one is a line"
    );
    assert_eq!(trail[1].level, "INFO");
    assert_eq!(
        trail[2].field("high_events_delta"),
        Some("10"),
        "the recurrence is measured from the previous probe, not from the \
         throttle's whole life, so two incidents do not read as one"
    );
}

/// A throttle that never clears is re-reported on the tenth sample and not
/// before. Re-reporting every sample would bury the log at one line a minute
/// forever; reporting once and never again would leave a unit throttled for
/// hours looking like it recovered.
#[test]
fn a_throttle_that_persists_is_reported_on_the_tenth_sample_and_not_before() {
    let over = || pressure(3_400_000_000, 3_000_000_000, 1);
    let script: Vec<Option<CgroupPressure>> = (0..=RELOG_EVERY).map(|_| over()).collect();
    let (mut collector, clock, _) = collector(script);

    let trail = throttle_trail(|| {
        for index in 1..=RELOG_EVERY as i64 {
            beat(&mut collector, &clock, index);
        }
    });

    assert_eq!(
        trail.len(),
        2,
        "the crossing and the tenth sample, and nothing between them: {trail:?}"
    );
    assert!(
        trail.iter().all(|event| event.message == EXCEEDED),
        "a throttle that never cleared has no clearing to report"
    );
}

/// The control for the whole file: a cgroup with no limit. `sample_cgroup_pressure`
/// answers `None` for an unlimited unit and for a host that is not on cgroup v2
/// at all, and a trail that reported a throttle in either case would put
/// `cgroup_memory_high_exceeded` on every non-Linux host's log.
#[test]
fn a_cgroup_with_no_memory_high_limit_never_reports_a_throttle() {
    let (mut collector, clock, probes) = collector(vec![None, None, None]);

    let trail = throttle_trail(|| {
        beat(&mut collector, &clock, 1);
        beat(&mut collector, &clock, 2);
        beat(&mut collector, &clock, 3);
    });

    assert_eq!(probes.load(Ordering::SeqCst), 3, "the probe still ran");
    assert!(
        trail.is_empty(),
        "no limit is no throttle, and reporting one would name a problem on \
         every unlimited host: {trail:?}"
    );
}

/// The probe rides the minute-long host sample, not the heartbeat. A beat inside
/// the interval reuses the cached sample and must not re-probe, so the trail
/// cannot become one line per beat and the delta cannot be split across probes
/// that never happened.
#[test]
fn a_beat_inside_the_sample_interval_does_not_probe_the_cgroup_again() {
    let (mut collector, clock, probes) = collector(vec![
        pressure(3_400_000_000, 3_000_000_000, 5),
        pressure(3_400_000_000, 3_000_000_000, 900),
    ]);

    let trail = throttle_trail(|| {
        beat(&mut collector, &clock, 1);
        // Forty seconds later: the same sample is still good, so nothing about
        // the throttle was re-read.
        clock.set(1_000_000 + 1 + 40_000);
        collector.collect();
        beat(&mut collector, &clock, 2);
    });

    assert_eq!(
        probes.load(Ordering::SeqCst),
        2,
        "a re-probe inside the interval would read the second script entry and \
         manufacture a delta nobody measured"
    );
    assert_eq!(trail.len(), 1, "{trail:?}");
    assert_eq!(
        trail[0].field("high_events_delta"),
        Some("0"),
        "the streak is still the first over-reading, so no second line is due"
    );
}
