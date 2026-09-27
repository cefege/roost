//! The counters and the bounded buffer the diagnostics RPCs report and drain.
//!
//! Reached as `core.services.telemetry`. `MiscMetrics` counts and
//! `DiagDebugLogBatch` drains the same buffer, so two instances would report
//! counters nobody is incrementing.
//!
//! `new()` takes nothing and must keep taking nothing: the retention and
//! sampling the operator configures are read at call time from
//! `core.services.boot`.
//!
//! KEY CARDINALITY IS CAPPED, AND THE PATH IS THE ONLY THING THAT CAN GROW IT.
//! An unmatched request path is chosen by the caller, so a probe that invents
//! a URL per request would otherwise grow this map one entry per request and
//! never stop. Past the cap every further key folds into [`OVERFLOW_KEY`]
//! (`telemetry.ts:10-11`).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::diagnostics::transcription::TranscriptionRuntime;

/// How many distinct labels one counter map holds.
pub const MAX_TELEMETRY_KEYS: usize = 256;

/// The label every counter past the cap folds into.
pub const OVERFLOW_KEY: &str = "<other>";

/// The telemetry one coordinator process collects.
#[derive(Debug)]
pub struct Telemetry {
    /// Settings → Voice: the stored Deepgram key and dictation language, and
    /// the lifecycle of the reachability probe behind `TranscriptionTest`.
    ///
    /// It is here rather than beside the counters because it is the same
    /// process-wide truth: a coordinator reporting a stale probe as a current
    /// one is the same defect as two sets of counters.
    pub transcription: TranscriptionRuntime,
    /// Completed requests per label.
    requests: Mutex<BTreeMap<String, u64>>,
    /// Completed 4xx/5xx responses per label.
    errors: Mutex<BTreeMap<String, u64>>,
    /// When this process started counting, which is what `uptime_ms` is from.
    started: Instant,
}

impl Telemetry {
    /// A process that has counted nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            transcription: TranscriptionRuntime::new(),
            requests: Mutex::new(BTreeMap::new()),
            errors: Mutex::new(BTreeMap::new()),
            started: Instant::now(),
        }
    }

    /// Record one completed response under a caller-chosen label.
    ///
    /// `path` is a URL pathname or a Connect `/<service>/<method>`, already
    /// collapsed by the caller when it is unbounded — a static success and an
    /// unmatched probe each contribute one fixed label, so a probe loop cannot
    /// amplify this map (`coord-factory.ts:206-216`).
    pub fn record_audit_telemetry(&self, path: &str, status: u16) {
        increment(&self.requests, path);
        if (400..600).contains(&status) {
            increment(&self.errors, path);
        }
    }

    /// The counters, as `MiscMetrics` reports them.
    ///
    /// A poisoned lock reads as zero requests rather than as a failed RPC: the
    /// numbers are a monitoring convenience, and refusing to answer them would
    /// turn a diagnostic into an outage.
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        let requests = flatten(&self.requests);
        let errors = flatten(&self.errors);
        MetricsSnapshot {
            uptime_ms: self.started.elapsed().as_millis() as u64,
            total_requests: requests.values().sum(),
            total_errors: errors.values().sum(),
            requests,
            errors,
        }
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

/// One process's counters, as the RPC hands them over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Milliseconds since this coordinator started counting.
    pub uptime_ms: u64,
    /// Completed requests per label.
    pub requests: BTreeMap<String, u64>,
    /// Completed 4xx/5xx responses per label.
    pub errors: BTreeMap<String, u64>,
    /// Every request counted, across every label.
    pub total_requests: u64,
    /// Every error counted, across every label.
    pub total_errors: u64,
}

/// Count one event, folding into [`OVERFLOW_KEY`] past the cap.
fn increment(counter: &Mutex<BTreeMap<String, u64>>, key: &str) {
    let Ok(mut counter) = counter.lock() else {
        return;
    };
    if !counter.contains_key(key) && counter.len() >= MAX_TELEMETRY_KEYS {
        *counter.entry(OVERFLOW_KEY.to_owned()).or_insert(0) += 1;
        return;
    }
    *counter.entry(key.to_owned()).or_insert(0) += 1;
}

fn flatten(counter: &Mutex<BTreeMap<String, u64>>) -> BTreeMap<String, u64> {
    counter.lock().map_or_else(|_| BTreeMap::new(), |held| held.clone())
}
