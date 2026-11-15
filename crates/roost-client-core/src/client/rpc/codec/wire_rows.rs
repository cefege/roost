//! Protobuf row → wire row, for every list the client hydrates and every live
//! delta that carries the same row.
//!
//! One copy on purpose: the bootstrap lists (`codec::response`) and the Sync
//! decoder (`sync::decode`) call these, so a hydrated row and a live row cannot
//! disagree about a field. Ported from v2's `sync-bootstrap-hydration.ts:89-201`
//! and `client/sync/sync-proto-adapters.ts`; the brands, the keeper runtime and
//! the capacity report go through `roost_protocol`'s own adapters.
//!
//! These convert and brand-check; they do NOT run a row's `check()` (label
//! non-empty, timestamps positive), because v2's converters do not either.

use roost_proto::{
    HostMetrics as PbHostMetrics, PairRequest as PbPairRequest, Worker as PbWorker,
    Workspace as PbWorkspace,
};
use roost_protocol::proto_adapters::{
    host_identity_from_proto, keeper_runtime_observation_from_proto,
    terminal_core_capacity_report_from_proto,
};
use roost_protocol::wire::{HostMetrics, SessionId, Worker, WorkerFp, Workspace, WorkspaceId};
use roost_protocol::{ProtocolError, ProtocolResult};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::store::mutations::PairRequest;

/// One worker row.
///
/// An invalid capacity report becomes `None` rather than failing the row, as in
/// v2 (`terminalCoreCapacityProtoToWire`): the report is informational, and a
/// bad one must not hide the machine.
pub fn worker_from_proto(proto: &PbWorker) -> ProtocolResult<Worker> {
    Ok(Worker {
        fp: WorkerFp::try_from(proto.fp.as_str())?,
        label: proto.label.clone(),
        os: wire_enum("worker.os", &proto.os)?,
        host_identity: host_identity_from_proto(proto.host_identity.as_option()),
        git_sha: proto.git_sha.clone(),
        host_metrics: proto
            .host_metrics
            .as_option()
            .map(host_metrics_from_proto)
            .transpose()?,
        registered_at_ms: signed("worker.registered_at_ms", proto.registered_at_ms)?,
        last_seen_ms: signed("worker.last_seen_ms", proto.last_seen_ms)?,
        reachable_addr: proto.reachable_addr.clone(),
        keeper_runtime: proto
            .keeper_runtime
            .as_option()
            .map(keeper_runtime_observation_from_proto)
            .transpose()?,
        terminal_core_capacity: proto.terminal_core_capacity.as_option().and_then(|report| {
            terminal_core_capacity_report_from_proto(report)
                .map_err(|error| {
                    tracing::warn!(
                        target: "sync",
                        worker_fp = %proto.fp,
                        %error,
                        "terminal core capacity report invalid; dropped from the worker row"
                    );
                })
                .ok()
        }),
    })
}

/// One host metrics sample.
pub fn host_metrics_from_proto(proto: &PbHostMetrics) -> ProtocolResult<HostMetrics> {
    Ok(HostMetrics {
        cpu_pct: proto.cpu_pct,
        mem_used_bytes: signed("host_metrics.mem_used_bytes", proto.mem_used_bytes)?,
        mem_total_bytes: signed("host_metrics.mem_total_bytes", proto.mem_total_bytes)?,
        disk_used_bytes: signed("host_metrics.disk_used_bytes", proto.disk_used_bytes)?,
        disk_total_bytes: signed("host_metrics.disk_total_bytes", proto.disk_total_bytes)?,
        net_rx_bps: signed("host_metrics.net_rx_bps", proto.net_rx_bps)?,
        net_tx_bps: signed("host_metrics.net_tx_bps", proto.net_tx_bps)?,
        sampled_at_ms: signed("host_metrics.sampled_at_ms", proto.sampled_at_ms)?,
    })
}

/// One workspace row.
pub fn workspace_from_proto(proto: &PbWorkspace) -> ProtocolResult<Workspace> {
    Ok(Workspace {
        id: WorkspaceId::try_from(proto.id.as_str())?,
        worker_fp: WorkerFp::try_from(proto.worker_fp.as_str())?,
        name: proto.name.clone(),
        folder_path: proto.folder_path.clone(),
        color: proto.color.clone(),
        position: i64::from(proto.position),
        version: signed("workspace.version", proto.version)?,
        created_at_ms: signed("workspace.created_at_ms", proto.created_at_ms)?,
        updated_at_ms: signed("workspace.updated_at_ms", proto.updated_at_ms)?,
        session_ids: proto
            .session_ids
            .iter()
            .map(|session_id| SessionId::try_from(session_id.as_str()))
            .collect::<ProtocolResult<Vec<_>>>()?,
    })
}

/// One pending tap-to-pair request (`sync-bootstrap-hydration.ts:180-196`).
pub fn pair_request_from_proto(proto: &PbPairRequest) -> ProtocolResult<PairRequest> {
    Ok(PairRequest {
        ephemeral_id: proto.ephemeral_id.clone(),
        label: proto.label.clone(),
        created_at_ms: signed("pair_request.created_at_ms", proto.created_at_ms)?,
        user_agent: proto.user_agent.clone(),
        client_browser: proto.client_browser.clone(),
        client_os: proto.client_os.clone(),
        client_device_type: proto.client_device_type.clone(),
        source_ip: proto.source_ip.clone(),
        country_code: proto.country_code.clone(),
        region: proto.region.clone(),
        city: proto.city.clone(),
        edge_identity_provider: proto.edge_identity_provider.clone(),
        edge_identity: proto.edge_identity.clone(),
        edge_identity_verified: proto.edge_identity_verified,
        expires_at_ms: signed("pair_request.expires_at_ms", proto.expires_at_ms)?,
    })
}

/// A proto `uint64` in a wire `i64`. v2 read these through `Number()`, which a
/// value past `i64::MAX` could never have survived either.
fn signed(field: &str, value: u64) -> ProtocolResult<i64> {
    i64::try_from(value).map_err(|_| {
        ProtocolError::new(field, format!("{value} does not fit a signed 64-bit field"))
    })
}

/// A string-typed enum column, parsed by the wire type's own serde names so the
/// accepted spellings live once, on the type.
fn wire_enum<T: DeserializeOwned>(field: &str, raw: &str) -> ProtocolResult<T> {
    serde_json::from_value(Value::String(raw.to_string()))
        .map_err(|_| ProtocolError::new(field, format!("unknown value {raw:?}")))
}
