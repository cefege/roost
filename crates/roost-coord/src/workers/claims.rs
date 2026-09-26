//! What a worker claimed, read off the wire: the register claims, the heartbeat
//! claims, and the one platform check both share.
//!
//! Ported from the claim-building halves of
//! `apps/coord/src/workers/handlers-workers.ts:87-109` and
//! `apps/coord/src/workers/handlers-workers-heartbeat.ts:33-79`.
//!
//! TRUNCATION HAPPENS HERE, BEFORE ANY WRITE. A label, a build sha and a
//! reachable address are all worker-controlled strings that reach an operator's
//! terminal through a fleet view, so each is cut to the persisted bound on the
//! way in rather than on the way out.
//!
//! A PROOF THAT DOES NOT VALIDATE IS NOT A PARSE FAILURE. A keeper observation
//! or a capacity report the build cannot read is recorded as a malformed claim
//! and handled by the beat itself, which clears the column and refuses -- v2
//! catches the same two conversions (`handlers-workers-heartbeat.ts:44-64`).

use connectrpc::{ConnectError, ErrorCode};
use roost_platform::host_platform::HostPlatform;
use roost_protocol::proto_adapters::{
    host_identity_from_proto, keeper_runtime_observation_from_proto,
    terminal_core_capacity_report_from_proto,
};
use roost_protocol::wire::{HostMetrics, WorkerOs};

use crate::events::persistence_input::{MAX_PERSISTED_UTF8_BYTES, truncate_persisted_utf8};

use super::heartbeat::{HeartbeatClaim, MalformedClaims};
use super::register::WorkerClaims;
/// Refuse a platform this build does not support, before anything is written.
pub fn checked_worker_os(os: Option<&str>) -> Result<Option<WorkerOs>, ConnectError> {
    let Some(os) = os else {
        return Ok(None);
    };
    let unsupported = || ConnectError::new(ErrorCode::InvalidArgument, "unsupported worker os");
    match HostPlatform::parse(os) {
        Ok(HostPlatform::MacOs) => Ok(Some(WorkerOs::Darwin)),
        Ok(HostPlatform::Linux) => Ok(Some(WorkerOs::Linux)),
        Ok(HostPlatform::Windows) => Ok(Some(WorkerOs::Win32)),
        Err(_) => Err(unsupported()),
    }
}

/// What a registering worker claimed, bounded.
pub fn register_claims(
    request: &roost_proto::WorkersRegisterRequest,
) -> Result<WorkerClaims, ConnectError> {
    Ok(WorkerClaims {
        label: request
            .label
            .as_deref()
            .map(|label| bounded(label).to_owned()),
        os: checked_worker_os(request.os.as_deref())?,
        git_sha: request
            .git_sha
            .as_deref()
            .map(|sha| bounded(sha).to_owned()),
        reachable_addr: request
            .reachable_addr
            .as_deref()
            .map(|address| bounded(address).to_owned()),
        host_identity: request
            .host_identity
            .as_option()
            .and_then(|identity| host_identity_from_proto(Some(identity))),
    })
}

/// What one beat claimed, bounded and validated.
pub fn heartbeat_claim(
    request: &roost_proto::WorkersHeartbeatRequest,
) -> Result<HeartbeatClaim, ConnectError> {
    let keeper_runtime = request
        .keeper_runtime
        .as_option()
        .map(keeper_runtime_observation_from_proto);
    let terminal_core_capacity = request
        .terminal_core_capacity
        .as_option()
        .map(terminal_core_capacity_report_from_proto);
    let malformed = MalformedClaims {
        keeper_runtime: keeper_runtime.as_ref().is_some_and(Result::is_err),
        terminal_core_capacity: terminal_core_capacity.as_ref().is_some_and(Result::is_err),
    };
    Ok(HeartbeatClaim {
        host_metrics: request.host_metrics.as_option().map(host_metrics_of),
        git_sha: request
            .git_sha
            .as_deref()
            .map(|sha| bounded(sha).to_owned()),
        // An empty address means the beat could not resolve one, which keeps the
        // prior value rather than nulling a good one.
        reachable_addr: request
            .reachable_addr
            .as_deref()
            .filter(|address| !address.is_empty())
            .map(|address| bounded(address).to_owned()),
        os: checked_worker_os(request.os.as_deref())?,
        host_identity: request
            .host_identity
            .as_option()
            .map(|identity| host_identity_from_proto(Some(identity))),
        keeper_runtime: keeper_runtime.and_then(Result::ok),
        terminal_core_capacity: terminal_core_capacity.and_then(Result::ok),
        malformed,
    })
}

/// A load sample, as the column that stores it.
fn host_metrics_of(metrics: &roost_proto::HostMetrics) -> HostMetrics {
    HostMetrics {
        cpu_pct: metrics.cpu_pct,
        mem_used_bytes: signed(metrics.mem_used_bytes),
        mem_total_bytes: signed(metrics.mem_total_bytes),
        disk_used_bytes: signed(metrics.disk_used_bytes),
        disk_total_bytes: signed(metrics.disk_total_bytes),
        net_rx_bps: signed(metrics.net_rx_bps),
        net_tx_bps: signed(metrics.net_tx_bps),
        sampled_at_ms: signed(metrics.sampled_at_ms),
    }
}

/// A `uint64` sample as the `i64` the stored JSON carries.
///
/// Saturating rather than wrapping: a value past `i64::MAX` is a corrupt
/// sample, and rendering it as a negative byte count would read as a machine
/// reporting negative memory.
fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A worker-controlled string, cut to the persisted bound.
fn bounded(value: &str) -> &str {
    truncate_persisted_utf8(value, MAX_PERSISTED_UTF8_BYTES)
}

/// A worker- or operator-supplied string, cut to the persisted bound and owned.
///
/// Owned because both callers store it: the register write binds it and the
/// rename write binds it, and neither holds the request past the statement.
#[must_use]
pub fn bounded_worker_text(value: &str) -> String {
    bounded(value).to_owned()
}
