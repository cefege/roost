//! Conversions between the live host snapshot and its generated protobuf form.
//! The worker link (`WLiveMetrics`) and the sync firehose
//! (`WorkerLiveMetricsFrame`) both carry this message. The rules live on
//! `LiveHostSnapshot::check`; this module maps fields only.

use roost_proto::{LiveHostSnapshot as PbLiveHostSnapshot, LiveProcess as PbLiveProcess};

use crate::ProtocolResult;
use crate::wire::live_metrics::{LiveHostSnapshot, LiveProcess};

/// Map a snapshot onto its generated form, refusing one the rules reject so
/// an invalid sample never reaches the wire.
pub fn live_host_snapshot_to_proto(
    snapshot: &LiveHostSnapshot,
) -> ProtocolResult<PbLiveHostSnapshot> {
    snapshot.check()?;
    Ok(PbLiveHostSnapshot {
        cpu_pct: snapshot.cpu_pct,
        mem_used_bytes: snapshot.mem_used_bytes,
        mem_total_bytes: snapshot.mem_total_bytes,
        disk_used_bytes: snapshot.disk_used_bytes,
        disk_total_bytes: snapshot.disk_total_bytes,
        cpu_count: snapshot.cpu_count,
        uptime_secs: snapshot.uptime_secs,
        process_count: snapshot.process_count,
        processes: snapshot.processes.iter().map(process_to_proto).collect(),
        sampled_at_ms: snapshot.sampled_at_ms,
        ..Default::default()
    })
}

/// Map a generated snapshot back, then hold it to the same rules as a locally
/// built one.
pub fn live_host_snapshot_from_proto(
    snapshot: &PbLiveHostSnapshot,
) -> ProtocolResult<LiveHostSnapshot> {
    let decoded = LiveHostSnapshot {
        cpu_pct: snapshot.cpu_pct,
        mem_used_bytes: snapshot.mem_used_bytes,
        mem_total_bytes: snapshot.mem_total_bytes,
        disk_used_bytes: snapshot.disk_used_bytes,
        disk_total_bytes: snapshot.disk_total_bytes,
        cpu_count: snapshot.cpu_count,
        uptime_secs: snapshot.uptime_secs,
        process_count: snapshot.process_count,
        processes: snapshot.processes.iter().map(process_from_proto).collect(),
        sampled_at_ms: snapshot.sampled_at_ms,
    };
    decoded.check()?;
    Ok(decoded)
}

fn process_to_proto(process: &LiveProcess) -> PbLiveProcess {
    PbLiveProcess {
        pid: process.pid,
        ppid: process.ppid,
        name: process.name.clone(),
        cpu_pct: process.cpu_pct,
        rss_bytes: process.rss_bytes,
        ..Default::default()
    }
}

fn process_from_proto(process: &PbLiveProcess) -> LiveProcess {
    LiveProcess {
        pid: process.pid,
        ppid: process.ppid,
        name: process.name.clone(),
        cpu_pct: process.cpu_pct,
        rss_bytes: process.rss_bytes,
    }
}
