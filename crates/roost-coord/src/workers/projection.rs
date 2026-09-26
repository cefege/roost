//! The two projections of a `workers` row: the protobuf a browser renders, and
//! the wire presence a Sync socket folds.
//!
//! Ported from `packages/protocol/src/wire/row-proto.ts:45-129`. They read the
//! SAME row and must not drift: a field that reaches the browser but not the
//! presence frame is a fleet view that disagrees with itself one heartbeat later.
//!
//! AN UNREADABLE STORED FIELD IS ABSENT, NOT AN ERROR, and the reason is v2's
//! `safeJsonParse(row.host_metrics_json, null, ...)`: a row whose JSON no longer
//! decodes renders without that field rather than failing the whole list. The
//! re-validation inside the protobuf projection IS an error, because v2's
//! `keeperRuntimeToProto` re-parses the already-decoded value and throws.

use roost_proto::buffa::MessageField;
use roost_protocol::json::safe_json_parse;
use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::proto_adapters::{
    host_identity_to_proto, keeper_runtime_observation_to_proto,
    terminal_core_capacity_report_to_proto,
};
use roost_protocol::wire::{
    HostIdentity, HostMetrics, TerminalCoreCapacityReport, Worker as WireWorker, WorkerFp,
    normalize_host_identity,
};
use serde_json::Value;

use super::rows::{StoredWorkerRow, worker_os};

/// Why a stored row could not be projected.
#[derive(Debug, thiserror::Error)]
#[error("worker {fp}: {source}")]
pub struct WorkerProjectionError {
    /// The row that could not be projected.
    pub fp: String,
    /// What is wrong with it.
    #[source]
    pub source: roost_protocol::ProtocolError,
}

/// The row as the browser's `Worker` message.
pub fn worker_row_to_proto(
    row: &StoredWorkerRow,
) -> Result<roost_proto::Worker, WorkerProjectionError> {
    let os = worker_os(row).ok_or_else(|| WorkerProjectionError {
        fp: row.fp.clone(),
        source: roost_protocol::ProtocolError::new(
            "worker.os",
            format!("{:?} is not a supported platform", row.os),
        ),
    })?;
    let host_identity = host_identity_from_json(row.host_identity_json.as_deref());
    let keeper_runtime = decode_keeper_runtime(row.keeper_runtime_json.as_deref());
    let terminal_core_capacity =
        decode_terminal_core_capacity(row.terminal_core_capacity_json.as_deref());
    let project = |source: roost_protocol::ProtocolError| WorkerProjectionError {
        fp: row.fp.clone(),
        source,
    };
    Ok(roost_proto::Worker {
        fp: row.fp.clone(),
        label: row.label.clone(),
        os: os.as_str().to_owned(),
        host_identity: host_identity
            .as_ref()
            .map(|identity| MessageField::some(host_identity_to_proto(Some(identity))))
            .unwrap_or_else(MessageField::none),
        git_sha: row.git_sha.clone(),
        host_metrics: decode_host_metrics(row.host_metrics_json.as_deref())
            .map(|metrics| MessageField::some(host_metrics_to_proto(&metrics)))
            .unwrap_or_else(MessageField::none),
        registered_at_ms: as_u64(row.registered_at_ms),
        last_seen_ms: as_u64(row.last_seen_ms),
        reachable_addr: row.reachable_addr.clone(),
        keeper_runtime: keeper_runtime
            .as_ref()
            .map(|observation| {
                keeper_runtime_observation_to_proto(observation).map(MessageField::some)
            })
            .transpose()
            .map_err(project)?
            .unwrap_or_else(MessageField::none),
        terminal_core_capacity: terminal_core_capacity
            .as_ref()
            .map(|report| terminal_core_capacity_report_to_proto(report).map(MessageField::some))
            .transpose()
            .map_err(project)?
            .unwrap_or_else(MessageField::none),
        ..Default::default()
    })
}

/// The row as the Sync presence frame's worker record.
pub fn worker_row_to_wire_presence(
    row: &StoredWorkerRow,
) -> Result<WireWorker, WorkerProjectionError> {
    let project = |source: roost_protocol::ProtocolError| WorkerProjectionError {
        fp: row.fp.clone(),
        source,
    };
    Ok(WireWorker {
        fp: WorkerFp::try_from(row.fp.as_str()).map_err(project)?,
        label: row.label.clone(),
        os: worker_os(row).ok_or_else(|| {
            project(roost_protocol::ProtocolError::new(
                "worker.os",
                format!("{:?} is not a supported platform", row.os),
            ))
        })?,
        host_identity: host_identity_from_json(row.host_identity_json.as_deref()),
        git_sha: row.git_sha.clone(),
        host_metrics: decode_host_metrics(row.host_metrics_json.as_deref()),
        registered_at_ms: row.registered_at_ms,
        last_seen_ms: row.last_seen_ms,
        reachable_addr: row.reachable_addr.clone(),
        keeper_runtime: decode_keeper_runtime(row.keeper_runtime_json.as_deref()),
        terminal_core_capacity: decode_terminal_core_capacity(
            row.terminal_core_capacity_json.as_deref(),
        ),
    })
}

/// The last sampled load, or `None` when the column is empty or unreadable.
#[must_use]
pub fn decode_host_metrics(stored: Option<&str>) -> Option<HostMetrics> {
    let value = safe_json_parse(stored, Value::Null);
    serde_json::from_value::<HostMetrics>(value).ok()
}

/// The static machine identity, normalized, or `None`.
#[must_use]
pub fn host_identity_from_json(stored: Option<&str>) -> Option<HostIdentity> {
    normalize_host_identity(&safe_json_parse(stored, Value::Null))
}

/// The authenticated keeper proof, or `None` when it does not validate.
#[must_use]
pub fn decode_keeper_runtime(stored: Option<&str>) -> Option<KeeperRuntimeObservationV1> {
    KeeperRuntimeObservationV1::parse(&safe_json_parse(stored, Value::Null)).ok()
}

/// The worker's terminal-core admission report, or `None`.
#[must_use]
pub fn decode_terminal_core_capacity(stored: Option<&str>) -> Option<TerminalCoreCapacityReport> {
    TerminalCoreCapacityReport::parse(safe_json_parse(stored, Value::Null)).ok()
}

/// The sampled load as the protobuf a browser reads.
#[must_use]
fn host_metrics_to_proto(metrics: &HostMetrics) -> roost_proto::HostMetrics {
    roost_proto::HostMetrics {
        cpu_pct: metrics.cpu_pct,
        mem_used_bytes: as_u64(metrics.mem_used_bytes),
        mem_total_bytes: as_u64(metrics.mem_total_bytes),
        disk_used_bytes: as_u64(metrics.disk_used_bytes),
        disk_total_bytes: as_u64(metrics.disk_total_bytes),
        net_rx_bps: as_u64(metrics.net_rx_bps),
        net_tx_bps: as_u64(metrics.net_tx_bps),
        sampled_at_ms: as_u64(metrics.sampled_at_ms),
        ..Default::default()
    }
}

/// A stored millisecond stamp as the `uint64` the wire carries.
///
/// Saturating, because a negative stamp is a corrupt row and a `0` renders as
/// "never" next to a label, which is the honest reading; a wrapped `u64` would
/// render as a date in the year 584 billion.
fn as_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}
