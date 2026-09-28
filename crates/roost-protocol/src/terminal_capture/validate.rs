//! Envelope validation for a terminal incident bundle before anything replays
//! it, run by the worker's bundle writer as a WRITE-SIDE GATE. Ports
//! `packages/protocol/src/terminal-capture-validate.ts`; field primitives live
//! in `super::validate_fields`, per-layer sections in `super::validate_layers`.
//! A rejection is a fixed code plus a field path, never a message quoting the
//! value: every value validated here is terminal content.

use serde_json::{Map, Value};

use super::TerminalCaptureErrorCode::{EvidenceMalformed, InvalidArgument};
use super::bundle::{TERMINAL_INCIDENT_SCHEMA, TerminalCaptureLayer, TerminalCaptureReason};
use super::validate_fields::{
    CaptureFieldRefusal, FieldCheck, fail, is_count, is_decimal_uint64, is_epoch_ms,
    is_nullable_string, validate_stream_identity,
};
use super::validate_layers::{
    validate_browser_section, validate_coordinator_section, validate_worker_section,
};
use crate::viewport::is_terminal_uuid;

const LAYERS: [TerminalCaptureLayer; 3] = [
    TerminalCaptureLayer::Browser,
    TerminalCaptureLayer::Coordinator,
    TerminalCaptureLayer::Worker,
];
const COVERAGE_VALUES: [&str; 3] = ["complete", "partial", "unavailable"];
const COVERAGE_FIELDS: [&str; 3] = ["cell_replay", "core_replay", "core_comparison"];

/// Validate a whole bundle as it will be written (or was read back).
pub fn validate_terminal_incident_bundle(value: &Value) -> Result<(), CaptureFieldRefusal> {
    let Some(root) = value.as_object() else {
        return fail(EvidenceMalformed, "$");
    };
    if root.get("schema").and_then(Value::as_str) != Some(TERMINAL_INCIDENT_SCHEMA) {
        return fail(EvidenceMalformed, "schema");
    }
    for field in ["capture_id", "recording_id", "session_id"] {
        if !root
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(is_terminal_uuid)
        {
            return fail(InvalidArgument, field);
        }
    }
    if !is_epoch_ms(root.get("written_at_ms")) {
        return fail(InvalidArgument, "written_at_ms");
    }
    validate_trigger(root.get("trigger"))?;
    validate_coverage(root.get("coverage"))?;
    for layer in LAYERS {
        let name = layer.as_str();
        let section = match root.get(name) {
            None | Some(Value::Null) => continue,
            Some(Value::Object(section)) => section,
            Some(_) => return fail(EvidenceMalformed, name),
        };
        validate_layer_header(section, layer)?;
        match layer {
            TerminalCaptureLayer::Worker => validate_worker_section(section)?,
            TerminalCaptureLayer::Coordinator => validate_coordinator_section(section)?,
            TerminalCaptureLayer::Browser => validate_browser_section(section)?,
        }
    }
    Ok(())
}

/// Whether a literal names a layer.
fn is_layer(value: Option<&Value>) -> bool {
    let named = value.and_then(Value::as_str);
    LAYERS.iter().any(|layer| Some(layer.as_str()) == named)
}

fn validate_trigger(value: Option<&Value>) -> FieldCheck {
    let Some(trigger) = value.and_then(Value::as_object) else {
        return fail(EvidenceMalformed, "trigger");
    };
    if trigger
        .get("reason")
        .and_then(Value::as_str)
        .and_then(TerminalCaptureReason::parse)
        .is_none()
    {
        return fail(InvalidArgument, "trigger.reason");
    }
    if !is_layer(trigger.get("origin")) {
        return fail(InvalidArgument, "trigger.origin");
    }
    if !is_epoch_ms(trigger.get("at_ms")) {
        return fail(InvalidArgument, "trigger.at_ms");
    }
    for field in ["stream_id", "grid_epoch"] {
        if !is_nullable_string(trigger.get(field)) {
            return fail(InvalidArgument, format!("trigger.{field}"));
        }
    }
    let seq = trigger.get("seq");
    if seq != Some(&Value::Null) && !is_decimal_uint64(seq) {
        return fail(InvalidArgument, "trigger.seq");
    }
    if !is_nullable_string(trigger.get("detail")) {
        return fail(InvalidArgument, "trigger.detail");
    }
    if !is_count(trigger.get("occurrence_count")) {
        return fail(InvalidArgument, "trigger.occurrence_count");
    }
    Ok(())
}

/// A coverage axis with no reason is a claim with no basis, so an empty reason
/// list is rejected rather than read as "complete".
fn validate_coverage(value: Option<&Value>) -> FieldCheck {
    let Some(coverage) = value.and_then(Value::as_object) else {
        return fail(EvidenceMalformed, "coverage");
    };
    for field in COVERAGE_FIELDS {
        let named = coverage.get(field).and_then(Value::as_str);
        if !COVERAGE_VALUES.iter().any(|value| Some(*value) == named) {
            return fail(InvalidArgument, format!("coverage.{field}"));
        }
        let reasons = coverage
            .get(&format!("{field}_reasons"))
            .and_then(Value::as_array);
        if !reasons
            .is_some_and(|reasons| !reasons.is_empty() && reasons.iter().all(Value::is_string))
        {
            return fail(InvalidArgument, format!("coverage.{field}_reasons"));
        }
    }
    Ok(())
}

fn validate_layer_header(header: &Map<String, Value>, layer: TerminalCaptureLayer) -> FieldCheck {
    let name = layer.as_str();
    if header.get("layer").and_then(Value::as_str) != Some(name) {
        return fail(InvalidArgument, format!("{name}.layer"));
    }
    if !is_epoch_ms(header.get("captured_at_ms")) {
        return fail(InvalidArgument, format!("{name}.captured_at_ms"));
    }
    let process = header.get("process").and_then(Value::as_object);
    let Some(process) =
        process.filter(|process| process.get("layer").and_then(Value::as_str) == Some(name))
    else {
        return fail(EvidenceMalformed, format!("{name}.process"));
    };
    for field in ["process_id", "git_sha", "artifact_version"] {
        if !process.get(field).is_some_and(Value::is_string) {
            return fail(InvalidArgument, format!("{name}.process.{field}"));
        }
    }
    if !matches!(header.get("stream"), None | Some(Value::Null)) {
        validate_stream_identity(header.get("stream"), &format!("{name}.stream"))?;
    }
    let Some(dropped) = header.get("dropped").and_then(Value::as_object) else {
        return fail(EvidenceMalformed, format!("{name}.dropped"));
    };
    for field in ["records", "bytes", "rows", "raw_bytes", "samples"] {
        if !is_count(dropped.get(field)) {
            return fail(InvalidArgument, format!("{name}.dropped.{field}"));
        }
    }
    if !header.get("omissions").is_some_and(Value::is_array) {
        return fail(InvalidArgument, format!("{name}.omissions"));
    }
    Ok(())
}
