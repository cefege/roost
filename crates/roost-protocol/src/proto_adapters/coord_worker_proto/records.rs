//! The record-to-protobuf conversions the two link directions share: the
//! agent-status observation, the update-progress report, the terminal
//! metadata frame, and the scalar records whose wire fields are narrower than
//! the domain ones. Called by `coord_worker_proto::upstream` and
//! `coord_worker_proto::downstream`.
//!
//! The agent-status mapping goes through the domain type's own serde form
//! rather than naming the state and source strings here: those spellings are
//! the `#[serde(rename_all)]` on `AgentRuntimeState` and `AgentStatusSource`,
//! and a second copy of them is exactly the fork this crate exists to prevent.

use serde_json::{Value, json};

use roost_proto::{WAgentStatus, WTerminalMetadata, WUpdateProgress};

use crate::wire::agent_status::{AgentStatus, AgentStatusSource};
use crate::wire::brand::ChannelId;
use crate::wire::coord_worker::{AgentStatusFrame, TerminalMetadata, UpdateProgress};
use crate::{ProtocolError, ProtocolResult};

/// A `uint64` the domain union's `i64` cannot hold, refused rather than
/// wrapped: a wrapped sequence is a different event's sequence.
pub(super) fn narrow_i64(field: &str, value: u64) -> ProtocolResult<i64> {
    i64::try_from(value).map_err(|_| {
        ProtocolError::new(field, format!("must not exceed {}, got {value}", i64::MAX))
    })
}

/// The reverse. A negative timestamp is not something `uint64` can carry, and
/// sending it as a wrapped number would move it centuries into the future.
pub(super) fn widen_u64(field: &str, value: i64) -> ProtocolResult<u64> {
    u64::try_from(value)
        .map_err(|_| ProtocolError::new(field, format!("must be non-negative, got {value}")))
}

/// A channel id off the wire, held to the same shape the domain brand enforces.
pub(super) fn channel_id(field: &str, value: u32) -> ProtocolResult<ChannelId> {
    ChannelId::try_from(i64::from(value)).map_err(|error| error.within(field))
}

/// The proto carries `updated_at` as a double; the domain owns it as whole
/// milliseconds. A fractional or out-of-range value is refused rather than
/// truncated, because a status stamped at the wrong millisecond fences against
/// the wrong revision.
fn updated_at_to_i64(value: f64) -> ProtocolResult<i64> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(ProtocolError::new(
            "agent_status.updated_at",
            format!("must be a whole number of milliseconds, got {value}"),
        ));
    }
    if value < i64::MIN as f64 || value > i64::MAX as f64 {
        return Err(ProtocolError::new(
            "agent_status.updated_at",
            format!("must fit in 64 signed bits, got {value}"),
        ));
    }
    Ok(value as i64)
}

fn updated_at_to_f64(value: i64) -> ProtocolResult<f64> {
    // An i64 beyond 2^53 has no exact double, so it would come back as a
    // different millisecond than it left as.
    if value.unsigned_abs() > (1u64 << 53) {
        return Err(ProtocolError::new(
            "agent_status.updated_at",
            format!("{value} milliseconds is not exactly representable on the wire"),
        ));
    }
    Ok(value as f64)
}

/// One status field as the domain type spells it on the wire.
fn enum_string<T: serde::Serialize>(field: &str, value: T) -> ProtocolResult<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| ProtocolError::new(field, "has no wire spelling"))
}

fn enum_from_str<T: serde::de::DeserializeOwned>(field: &str, value: &str) -> ProtocolResult<T> {
    serde_json::from_value(json!(value)).map_err(|_| {
        ProtocolError::new(field, format!("{value:?} is not a value this crate names"))
    })
}

/// A volatile agent-status observation, in both directions.
pub(super) fn agent_status_to_proto(frame: &AgentStatusFrame) -> ProtocolResult<WAgentStatus> {
    let status = &frame.status;
    let common = &status.common;
    Ok(WAgentStatus {
        session_id: common.session_id.as_str().to_owned(),
        agent_id: common.agent_id.as_str().to_owned(),
        state: enum_string("agent_status.state", common.state)?,
        message: common.message.clone(),
        revision: widen_u64("agent_status.revision", common.revision)?,
        completed_revision: widen_u64(
            "agent_status.completed_revision",
            common.completed_revision,
        )?,
        updated_at: updated_at_to_f64(common.updated_at)?,
        active: status.active,
        status_epoch: common
            .status_epoch
            .as_ref()
            .map(|epoch| epoch.as_str().to_owned()),
        occupant_id: common
            .occupant_id
            .as_ref()
            .map(|occupant| occupant.as_str().to_owned()),
        source: common.source.map(|source| source.as_str().to_owned()),
        occupant_exited: common.occupant_exited,
        ..Default::default()
    })
}

pub(super) fn agent_status_from_proto(status: &WAgentStatus) -> ProtocolResult<AgentStatus> {
    let source = match &status.source {
        Some(spelling) => Some(enum_from_str::<AgentStatusSource>(
            "agent_status.source",
            spelling,
        )?),
        None => None,
    };
    // The domain type's own parse is the admission path: it re-runs the field
    // bounds and refuses an inactive status, which is an update rather than a
    // retained row.
    let value = json!({
        "session_id": status.session_id,
        "agent_id": status.agent_id,
        "state": status.state,
        "message": status.message,
        "revision": narrow_i64("agent_status.revision", status.revision)?,
        "completed_revision": narrow_i64("agent_status.completed_revision", status.completed_revision)?,
        "updated_at": updated_at_to_i64(status.updated_at)?,
        "status_epoch": status.status_epoch,
        "occupant_id": status.occupant_id,
        "source": source.map(|source| source.as_str().to_owned()),
        "occupant_exited": status.occupant_exited,
        "active": status.active,
    });
    AgentStatus::parse(value)
}

/// Journal-backed update progress, in both directions.
pub(super) fn update_progress_to_proto(
    progress: &UpdateProgress,
) -> ProtocolResult<WUpdateProgress> {
    Ok(WUpdateProgress {
        request_id: progress.request_id.clone(),
        job_id: progress.job_id.clone(),
        sequence: progress.sequence,
        phase: progress.phase.clone(),
        message: progress.message.clone(),
        terminal: progress.terminal,
        success: progress.success,
        error: progress.error.clone(),
        ..Default::default()
    })
}

pub(super) fn update_progress_from_proto(progress: &WUpdateProgress) -> UpdateProgress {
    UpdateProgress {
        request_id: progress.request_id.clone(),
        job_id: progress.job_id.clone(),
        sequence: progress.sequence,
        phase: progress.phase.clone(),
        message: progress.message.clone(),
        terminal: progress.terminal,
        success: progress.success,
        error: progress.error.clone(),
    }
}

/// Compact terminal metadata, in both directions. The two `changed` booleans
/// are what separate "the title is now empty" from "the title did not change
/// on this frame", so they travel rather than being inferred.
pub(super) fn metadata_to_proto(metadata: &TerminalMetadata) -> ProtocolResult<WTerminalMetadata> {
    Ok(WTerminalMetadata {
        channel_id: metadata.channel_id.as_u32(),
        title_changed: metadata.title_changed,
        title: metadata.title.clone(),
        activity_changed: metadata.activity_changed,
        activity_ts_ms: metadata.activity_ts_ms,
        ..Default::default()
    })
}

pub(super) fn metadata_from_proto(
    metadata: &WTerminalMetadata,
) -> ProtocolResult<TerminalMetadata> {
    Ok(TerminalMetadata {
        channel_id: channel_id("terminal_metadata.channel_id", metadata.channel_id)?,
        title_changed: metadata.title_changed,
        title: metadata.title.clone(),
        activity_changed: metadata.activity_changed,
        activity_ts_ms: metadata.activity_ts_ms,
    })
}

/// The JSON payload of an `rpc-ok`, which the wire carries as a string.
pub(super) fn data_to_json(data: &Value) -> ProtocolResult<String> {
    serde_json::to_string(data)
        .map_err(|error| ProtocolError::new("rpc-ok.data", error.to_string()))
}

pub(super) fn data_from_json(field: &str, raw: &str) -> ProtocolResult<Value> {
    serde_json::from_str(raw).map_err(|error| ProtocolError::new(field, error.to_string()))
}
