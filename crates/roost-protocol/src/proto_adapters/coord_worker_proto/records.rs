//! The record-to-protobuf conversions the two link directions share: the
//! update-progress report, terminal metadata, and scalar records whose wire
//! fields are narrower than the domain ones. Called by `coord_worker_proto`
//! link direction adapters.

use serde_json::Value;

use roost_proto::{
    WTerminalMetadata, WTerminalNotification, WTerminalProgress, WTerminalUserVar, WUpdateProgress,
};

use crate::proto_adapters::program_status_proto::{
    program_status_records_from_proto, program_status_records_to_proto,
};
use crate::terminal_signals::{TerminalNotification, TerminalProgress, TerminalUserVar};
use crate::wire::brand::ChannelId;
use crate::wire::coord_worker::{TerminalMetadata, UpdateProgress};
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

/// Compact terminal metadata, in both directions. Changed flags distinguish
/// empty values from unchanged fields; clipboard text is emitted as an event,
/// never inferred from a retained title/activity snapshot.
pub(super) fn metadata_to_proto(metadata: &TerminalMetadata) -> ProtocolResult<WTerminalMetadata> {
    Ok(WTerminalMetadata {
        channel_id: metadata.channel_id.as_u32(),
        title_changed: metadata.title_changed,
        title: metadata.title.clone(),
        activity_changed: metadata.activity_changed,
        activity_ts_ms: metadata.activity_ts_ms,
        clipboard_changed: metadata.clipboard_changed,
        clipboard: metadata.clipboard.clone(),
        command_finished: metadata.command_finished,
        command_exit_code: metadata.command_exit_code,
        command_duration_ms: metadata.command_duration_ms,
        bell: metadata.bell,
        progress: match metadata.progress {
            Some(progress) => {
                let (state, percent) = progress.to_wire();
                roost_proto::buffa::MessageField::some(WTerminalProgress {
                    state,
                    percent,
                    ..Default::default()
                })
            }
            None => roost_proto::buffa::MessageField::none(),
        },
        notifications: metadata
            .notifications
            .iter()
            .map(|notification| WTerminalNotification {
                title: notification.title.clone(),
                body: notification.body.clone(),
                ..Default::default()
            })
            .collect(),
        user_vars_changed: metadata.user_vars_changed,
        user_vars: metadata
            .user_vars
            .iter()
            .map(|var| WTerminalUserVar {
                key: var.key.clone(),
                value: var.value.clone(),
                ..Default::default()
            })
            .collect(),
        program_status_changed: metadata.program_status.is_some(),
        program_status: metadata
            .program_status
            .as_deref()
            .map_or_else(Vec::new, program_status_records_to_proto),
        ..Default::default()
    })
}

pub(super) fn metadata_from_proto(
    metadata: &WTerminalMetadata,
) -> ProtocolResult<TerminalMetadata> {
    let program_status = if metadata.program_status_changed {
        Some(program_status_records_from_proto(&metadata.program_status)?)
    } else {
        None
    };
    Ok(TerminalMetadata {
        channel_id: channel_id("terminal_metadata.channel_id", metadata.channel_id)?,
        title_changed: metadata.title_changed,
        title: metadata.title.clone(),
        activity_changed: metadata.activity_changed,
        activity_ts_ms: metadata.activity_ts_ms,
        clipboard_changed: metadata.clipboard_changed,
        clipboard: metadata.clipboard.clone(),
        command_finished: metadata.command_finished,
        command_exit_code: metadata.command_exit_code,
        command_duration_ms: metadata.command_duration_ms,
        bell: metadata.bell,
        progress: match metadata.progress.as_option() {
            Some(progress) => Some(
                TerminalProgress::from_wire(progress.state, progress.percent).ok_or_else(|| {
                    ProtocolError::new(
                        "terminal_metadata.progress.state",
                        format!("{} is not an OSC 9;4 state", progress.state),
                    )
                })?,
            ),
            None => None,
        },
        notifications: metadata
            .notifications
            .iter()
            .map(|notification| TerminalNotification {
                title: notification.title.clone(),
                body: notification.body.clone(),
            })
            .collect(),
        user_vars_changed: metadata.user_vars_changed,
        user_vars: metadata
            .user_vars
            .iter()
            .map(|var| TerminalUserVar {
                key: var.key.clone(),
                value: var.value.clone(),
            })
            .collect(),
        program_status,
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
