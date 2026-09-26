//! The counters and the bounded buffer the diagnostics RPCs report and drain.
//!
//! Reached as `core.services.telemetry`. `MiscMetrics` counts and
//! `DiagDebugLogBatch` drains the same buffer, so two instances would report
//! counters nobody is incrementing.
//!
//! `new()` takes nothing and must keep taking nothing: the retention and
//! sampling the operator configures are read at call time from
//! `core.services.boot`.

/// The telemetry one coordinator process collects.
#[derive(Debug, Default)]
pub struct Telemetry;

impl Telemetry {
    /// A process that has counted nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
