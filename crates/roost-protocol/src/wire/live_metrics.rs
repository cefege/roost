//! The live host snapshot a worker samples while a coordinator holds a live
//! subscription: host CPU, memory, disk, and a bounded process list. The worker
//! link (`WLiveMetrics`) and the sync firehose (`WorkerLiveMetricsFrame`) carry
//! this one model, so both directions share one set of rules.

use serde::{Deserialize, Serialize};

use crate::{ProtocolError, ProtocolResult};

/// The longest process list a snapshot may carry: the top 50 by CPU unioned
/// with the top 50 by resident memory.
pub const LIVE_PROCESS_LIST_MAX: usize = 100;

/// One process in a live snapshot. `name` is the command name only, never the
/// argument vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveProcess {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// Per-core share, so a multi-threaded process may exceed 100.
    pub cpu_pct: f64,
    pub rss_bytes: u64,
}

/// One host sample. `processes` is a subset of the host's processes, and
/// `process_count` is the host's full count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveHostSnapshot {
    /// Whole-host CPU share, 0 through 100.
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_total_bytes: u64,
    pub cpu_count: u32,
    pub uptime_secs: u64,
    pub process_count: u32,
    pub processes: Vec<LiveProcess>,
    pub sampled_at_ms: u64,
}

impl LiveHostSnapshot {
    /// The rules a snapshot is held to beyond its field types.
    pub fn check(&self) -> ProtocolResult<()> {
        if !(self.cpu_pct.is_finite() && (0.0..=100.0).contains(&self.cpu_pct)) {
            return Err(ProtocolError::new(
                "live_metrics.cpu_pct",
                format!("{} is not a 0-100 host CPU percent", self.cpu_pct),
            ));
        }
        if self.cpu_count == 0 {
            return Err(ProtocolError::new(
                "live_metrics.cpu_count",
                "a host has at least one CPU",
            ));
        }
        if self.sampled_at_ms == 0 {
            return Err(ProtocolError::new(
                "live_metrics.sampled_at_ms",
                "a sample carries a positive epoch millisecond",
            ));
        }
        if self.processes.len() > LIVE_PROCESS_LIST_MAX {
            return Err(ProtocolError::new(
                "live_metrics.processes",
                format!(
                    "lists {} processes, the limit is {LIVE_PROCESS_LIST_MAX}",
                    self.processes.len()
                ),
            ));
        }
        let listed = u32::try_from(self.processes.len()).unwrap_or(u32::MAX);
        if self.process_count < listed {
            return Err(ProtocolError::new(
                "live_metrics.process_count",
                format!(
                    "reports {} processes but lists {}",
                    self.process_count,
                    self.processes.len()
                ),
            ));
        }
        for (index, process) in self.processes.iter().enumerate() {
            if !(process.cpu_pct.is_finite() && process.cpu_pct >= 0.0) {
                return Err(ProtocolError::new(
                    format!("live_metrics.processes[{index}].cpu_pct"),
                    format!("{} is not a non-negative CPU share", process.cpu_pct),
                ));
            }
        }
        Ok(())
    }
}
