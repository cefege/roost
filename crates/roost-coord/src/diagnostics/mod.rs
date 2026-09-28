//! The diagnostics domain: audit rows, metrics, the debug log batch, the
//! telemetry that feeds them, and the transcription configuration.
//!
//! One field on `CoordServices` for the telemetry, so the counters a metric
//! reports and the buffer a debug-log batch drains are one process's.

pub mod diag_log;
pub mod diag_snapshot;
pub mod rpc_audit;
pub mod rpc_metrics;
pub mod rpc_transcription;
pub mod session_state;
pub mod telemetry;
pub mod transcription;
pub mod worker_results;
