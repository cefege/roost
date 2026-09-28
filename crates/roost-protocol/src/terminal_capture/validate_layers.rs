//! Per-layer section validation for an incident bundle: worker, coordinator and
//! browser. Ports `packages/protocol/src/terminal-capture-validate-layers.ts`;
//! called only by `super::validate` after the envelope and the layer header
//! passed. It proves shape, bounds and per-segment sequence continuity so a
//! replay conclusion rests on an orderable record set, and reads no text.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use super::validate_fields::{
    FieldCheck, as_record, decimal_value, fail, is_decimal_uint64, is_dimension, is_epoch_ms,
    validate_frame, validate_row, validate_stream_identity,
};
use super::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode};

use TerminalCaptureErrorCode::{EvidenceMalformed, InvalidArgument, ResourceExhausted};

const WORKER_BOUNDED_ARRAYS: [&str; 5] = [
    "emissions",
    "core_samples",
    "resizes",
    "raw",
    "history_ranges",
];
const BROWSER_STATES: [&str; 4] = [
    "trigger_state",
    "pre_repair_state",
    "post_repair_state",
    "current_state",
];

/// An array member, or the empty slice for one v2 already proved is an array.
fn entries<'a>(section: &'a Map<String, Value>, name: &str) -> &'a [Value] {
    section
        .get(name)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

pub(crate) fn validate_worker_section(section: &Map<String, Value>) -> FieldCheck {
    let Some(segments) = section.get("segments").and_then(Value::as_array) else {
        return fail(InvalidArgument, "worker.segments");
    };
    let mut segment_ids = HashSet::with_capacity(segments.len());
    for (position, raw) in segments.iter().enumerate() {
        let Some(segment_id) = raw
            .as_object()
            .and_then(|segment| segment.get("segment_id"))
            .and_then(Value::as_str)
        else {
            return fail(EvidenceMalformed, format!("worker.segments[{position}]"));
        };
        if !is_decimal_uint64(raw.get("open_offset")) {
            return fail(
                InvalidArgument,
                format!("worker.segments[{position}].open_offset"),
            );
        }
        if !is_epoch_ms(raw.get("opened_at_ms")) {
            return fail(
                InvalidArgument,
                format!("worker.segments[{position}].opened_at_ms"),
            );
        }
        segment_ids.insert(segment_id);
    }
    for name in WORKER_BOUNDED_ARRAYS {
        let Some(bounded) = section.get(name).and_then(Value::as_array) else {
            return fail(InvalidArgument, format!("worker.{name}"));
        };
        if bounded.len() > TERMINAL_CAPTURE_LIMITS.layer_entries {
            return fail(ResourceExhausted, format!("worker.{name}"));
        }
    }
    validate_worker_emissions(entries(section, "emissions"), &segment_ids)?;
    for (position, raw) in entries(section, "core_samples").iter().enumerate() {
        let Some(record) = raw.as_object() else {
            return fail(
                EvidenceMalformed,
                format!("worker.core_samples[{position}]"),
            );
        };
        validate_stream_identity(
            record.get("stream"),
            &format!("worker.core_samples[{position}].stream"),
        )?;
        for name in ["core_frame", "fold_frame"] {
            validate_frame(
                record.get(name),
                &format!("worker.core_samples[{position}].{name}"),
            )?;
        }
    }
    for (position, raw) in entries(section, "raw").iter().enumerate() {
        validate_raw_record(raw, position)?;
    }
    for (position, raw) in entries(section, "resizes").iter().enumerate() {
        validate_resize_record(raw, position)?;
    }
    for name in ["core_scrollback_tail", "history_rows"] {
        let Some(rows) = section.get(name).and_then(Value::as_array) else {
            return fail(InvalidArgument, format!("worker.{name}"));
        };
        for (position, row) in rows.iter().enumerate() {
            validate_row(Some(row), &format!("worker.{name}[{position}]"))?;
        }
    }
    Ok(())
}

fn validate_raw_record(raw: &Value, position: usize) -> FieldCheck {
    let Some(record) = raw.as_object() else {
        return fail(EvidenceMalformed, format!("worker.raw[{position}]"));
    };
    if !is_decimal_uint64(record.get("start_offset")) {
        return fail(
            InvalidArgument,
            format!("worker.raw[{position}].start_offset"),
        );
    }
    if !is_decimal_uint64(record.get("end_offset")) {
        return fail(
            InvalidArgument,
            format!("worker.raw[{position}].end_offset"),
        );
    }
    // Cross-field: neither offset is individually wrong, so the pair is named.
    if decimal_value(record.get("end_offset")) < decimal_value(record.get("start_offset")) {
        return fail(InvalidArgument, format!("worker.raw[{position}].offsets"));
    }
    if !record.get("base64").is_some_and(Value::is_string) {
        return fail(InvalidArgument, format!("worker.raw[{position}].base64"));
    }
    Ok(())
}

fn validate_resize_record(raw: &Value, position: usize) -> FieldCheck {
    let Some(record) = raw.as_object() else {
        return fail(EvidenceMalformed, format!("worker.resizes[{position}]"));
    };
    if !is_decimal_uint64(record.get("install_offset")) {
        return fail(
            InvalidArgument,
            format!("worker.resizes[{position}].install_offset"),
        );
    }
    let boundary = record.get("boundary_offset");
    if boundary != Some(&Value::Null) && !is_decimal_uint64(boundary) {
        return fail(
            InvalidArgument,
            format!("worker.resizes[{position}].boundary_offset"),
        );
    }
    for side in ["from", "to"] {
        let geometry = as_record(record.get(side));
        if !geometry.is_some_and(|geometry| {
            is_dimension(geometry.get("cols")) && is_dimension(geometry.get("rows"))
        }) {
            return fail(
                InvalidArgument,
                format!("worker.resizes[{position}].{side}"),
            );
        }
    }
    Ok(())
}

/// A later emission in the SAME segment may never carry a lower sequence: the
/// retained fold would be unorderable. Across segments numbering restarts.
fn validate_worker_emissions(emissions: &[Value], segment_ids: &HashSet<&str>) -> FieldCheck {
    let mut last_seq_by_segment: HashMap<&str, u64> = HashMap::new();
    for (position, raw) in emissions.iter().enumerate() {
        let Some(record) = raw.as_object() else {
            return fail(EvidenceMalformed, format!("worker.emissions[{position}]"));
        };
        let Some(segment_id) = record
            .get("segment_id")
            .and_then(Value::as_str)
            .filter(|id| segment_ids.contains(id))
        else {
            return fail(
                InvalidArgument,
                format!("worker.emissions[{position}].segment_id"),
            );
        };
        validate_stream_identity(
            record.get("stream"),
            &format!("worker.emissions[{position}].stream"),
        )?;
        validate_frame(
            record.get("frame"),
            &format!("worker.emissions[{position}].frame"),
        )?;
        let seq = decimal_value(record.get("stream").and_then(|stream| stream.get("seq")));
        if last_seq_by_segment
            .get(segment_id)
            .is_some_and(|previous| seq < *previous)
        {
            return fail(
                InvalidArgument,
                format!("worker.emissions[{position}].stream.seq"),
            );
        }
        last_seq_by_segment.insert(segment_id, seq);
    }
    Ok(())
}

pub(crate) fn validate_coordinator_section(section: &Map<String, Value>) -> FieldCheck {
    let Some(records) = section.get("records").and_then(Value::as_array) else {
        return fail(InvalidArgument, "coordinator.records");
    };
    if records.len() > TERMINAL_CAPTURE_LIMITS.layer_entries {
        return fail(ResourceExhausted, "coordinator.records");
    }
    for (position, raw) in records.iter().enumerate() {
        let Some(record) = raw.as_object() else {
            return fail(
                EvidenceMalformed,
                format!("coordinator.records[{position}]"),
            );
        };
        validate_stream_identity(
            record.get("stream"),
            &format!("coordinator.records[{position}].stream"),
        )?;
        if record.get("canonical") != Some(&Value::Null) {
            validate_frame(
                record.get("canonical"),
                &format!("coordinator.records[{position}].canonical"),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn validate_browser_section(section: &Map<String, Value>) -> FieldCheck {
    let Some(events) = section.get("events").and_then(Value::as_array) else {
        return fail(InvalidArgument, "browser.events");
    };
    if events.len() > TERMINAL_CAPTURE_LIMITS.layer_entries {
        return fail(ResourceExhausted, "browser.events");
    }
    for name in BROWSER_STATES {
        let value = section.get(name);
        if matches!(value, None | Some(Value::Null)) {
            continue;
        }
        let Some(state) = as_record(value) else {
            return fail(EvidenceMalformed, format!("browser.{name}"));
        };
        validate_painted_state(state, name)?;
    }
    Ok(())
}

fn validate_painted_state(state: &Map<String, Value>, name: &str) -> FieldCheck {
    if !is_epoch_ms(state.get("at_ms")) {
        return fail(InvalidArgument, format!("browser.{name}.at_ms"));
    }
    if state.get("canonical") != Some(&Value::Null) {
        validate_frame(state.get("canonical"), &format!("browser.{name}.canonical"))?;
    }
    for rows in ["dom_history", "dom_viewport"] {
        let Some(listed) = state.get(rows).and_then(Value::as_array) else {
            return fail(InvalidArgument, format!("browser.{name}.{rows}"));
        };
        if listed.len() > TERMINAL_CAPTURE_LIMITS.browser_rows_max {
            return fail(ResourceExhausted, format!("browser.{name}.{rows}"));
        }
    }
    let Some(model_rows) = state.get("painted_model_history").and_then(Value::as_array) else {
        return fail(
            InvalidArgument,
            format!("browser.{name}.painted_model_history"),
        );
    };
    for (position, row) in model_rows.iter().enumerate() {
        validate_row(
            Some(row),
            &format!("browser.{name}.painted_model_history[{position}]"),
        )?;
    }
    Ok(())
}
