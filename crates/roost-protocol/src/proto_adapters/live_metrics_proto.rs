//! Conversions between the live metrics models and their generated protobuf
//! forms. The worker link (`WLiveMetrics`) and the sync firehose
//! (`WorkerLiveMetricsFrame`) both carry the snapshot message, and the sync
//! socket's `WorkerLiveSubscription` carries the fingerprints it wants. The
//! rules live on the models' `check`; this module maps fields and checks.

use roost_proto::buffa::MessageField;
use roost_proto::{
    LiveHostSnapshot as PbLiveHostSnapshot, LiveProcess as PbLiveProcess,
    WorkerLiveMetricsFrame as PbWorkerLiveMetricsFrame,
    WorkerLiveSubscription as PbWorkerLiveSubscription,
};

use crate::wire::brand::WorkerFp;
use crate::wire::live_metrics::{
    LiveHostSnapshot, LiveProcess, WorkerLiveMetricsFrame, WorkerLiveSubscription,
};
use crate::{ProtocolError, ProtocolResult};

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

/// Map a subscription onto its generated form, refusing a list with a repeated
/// fingerprint.
pub fn worker_live_subscription_to_proto(
    subscription: &WorkerLiveSubscription,
) -> ProtocolResult<PbWorkerLiveSubscription> {
    subscription.check()?;
    Ok(PbWorkerLiveSubscription {
        worker_fps: subscription
            .worker_fps
            .iter()
            .map(|worker_fp| worker_fp.as_str().to_owned())
            .collect(),
        ..Default::default()
    })
}

/// Map a generated subscription back. Each fingerprint passes its own shape
/// check, and the list then passes the same duplicate check as a local one.
pub fn worker_live_subscription_from_proto(
    subscription: &PbWorkerLiveSubscription,
) -> ProtocolResult<WorkerLiveSubscription> {
    let worker_fps = subscription
        .worker_fps
        .iter()
        .map(|worker_fp| WorkerFp::try_from(worker_fp.as_str()))
        .collect::<ProtocolResult<Vec<WorkerFp>>>()?;
    let decoded = WorkerLiveSubscription { worker_fps };
    decoded.check()?;
    Ok(decoded)
}

/// Map a sync live-metrics frame onto its generated form. The snapshot's own
/// check runs inside `live_host_snapshot_to_proto`.
pub fn worker_live_metrics_frame_to_proto(
    frame: &WorkerLiveMetricsFrame,
) -> ProtocolResult<PbWorkerLiveMetricsFrame> {
    Ok(PbWorkerLiveMetricsFrame {
        worker_fp: frame.worker_fp.as_str().to_owned(),
        snapshot: MessageField::some(live_host_snapshot_to_proto(&frame.snapshot)?),
        ..Default::default()
    })
}

/// Map a generated sync live-metrics frame back. A frame with no snapshot is
/// refused, and the snapshot then passes the same check as a local one.
pub fn worker_live_metrics_frame_from_proto(
    frame: &PbWorkerLiveMetricsFrame,
) -> ProtocolResult<WorkerLiveMetricsFrame> {
    let snapshot = frame.snapshot.as_option().ok_or_else(|| {
        ProtocolError::new(
            "worker_live_metrics.snapshot",
            "live metrics frame is missing its snapshot",
        )
    })?;
    Ok(WorkerLiveMetricsFrame {
        worker_fp: WorkerFp::try_from(frame.worker_fp.as_str())?,
        snapshot: live_host_snapshot_from_proto(snapshot)?,
    })
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
