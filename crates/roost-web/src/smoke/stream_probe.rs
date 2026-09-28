//! `terminalStreamProbe(sessionId)`'s join: the browser snapshot plus the
//! coordinator's layered `DiagSnapshot` answer, normalized into one stable
//! smoke-facing record in which a missing or malformed layer stays explicit
//! instead of reading as healthy. Native; the wasm32 adapter in
//! `smoke::stream_probe_host` makes the RPC. Ports
//! `apps/web/src/smoke/smokeTerminalStreamProbe.ts:28-40,79-174`.

use serde_json::{Map, Value, json};

/// Decode the coordinator's `snapshot_json`.
pub fn parse_coordinator_snapshot(snapshot_json: &str) -> Result<Value, String> {
    serde_json::from_str(snapshot_json)
        .map_err(|error| format!("coordinator diagnostic snapshot was invalid JSON: {error}"))
}

/// `normalizeTerminalStreamProbe`: v2's `TerminalStreamProbe` shape.
pub fn normalize_terminal_stream_probe(
    session_id: &str,
    browser: Value,
    raw_snapshot: &Value,
) -> Result<Value, String> {
    let root = record(raw_snapshot)
        .ok_or_else(|| "coordinator diagnostic snapshot was not an object".to_owned())?;
    let coord = root.get("coord").and_then(record);
    let coord_session = coord
        .and_then(|coord| coord.get("sessions"))
        .and_then(record)
        .and_then(|sessions| sessions.get(session_id))
        .and_then(record);
    let terminal_view = coord_session
        .and_then(|session| session.get("terminal_view"))
        .and_then(record);
    let terminal_control = terminal_view.map(|view| {
        let effective = view.get("effective").and_then(record);
        let viewer_inputs = coord_session
            .and_then(|session| session.get("viewers"))
            .filter(|viewers| viewers.is_array())
            .cloned()
            .unwrap_or(Value::Null);
        json!({
            "active_view_count": member(view, "activeViews"),
            "parked_view_count": member(view, "parkedViews"),
            "stream_id": member(view, "streamId"),
            "unavailable": member(view, "unavailable"),
            "effective_cols": effective.map_or(Value::Null, |geometry| member(geometry, "cols")),
            "effective_rows": effective.map_or(Value::Null, |geometry| member(geometry, "rows")),
            // The per-view geometry the effective size was minimized over, so a
            // spec proves the minimum against its actual inputs.
            "viewer_inputs": viewer_inputs,
        })
    });
    let worker_fp = coord_session
        .and_then(|session| session.get("route"))
        .and_then(record)
        .and_then(|route| route.get("worker_fp"))
        .and_then(Value::as_str);
    let envelope = worker_fp
        .and_then(|fp| root.get("workers").and_then(record)?.get(fp))
        .and_then(record);
    let status = envelope
        .and_then(|envelope| envelope.get("status"))
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "ok" | "error"))
        .unwrap_or("missing");
    let response_ms = envelope
        .and_then(|envelope| envelope.get("response_ms"))
        .and_then(finite);
    let worker_snapshot = envelope
        .filter(|_| status == "ok")
        .and_then(|envelope| envelope.get("snapshot"))
        .and_then(record);
    let worker_session = worker_snapshot
        .and_then(|snapshot| snapshot.get("sessions"))
        .and_then(record)
        .and_then(|sessions| sessions.get(session_id))
        .and_then(record);
    let worker_error = (status == "error").then(|| {
        let error = envelope
            .and_then(|envelope| envelope.get("error"))
            .and_then(record);
        json!({
            "code": error.and_then(|error| string_member(error, "code")),
            "message": error.and_then(|error| string_member(error, "message")),
        })
    });
    let captured_at_ms = root
        .get("captured_at_ms")
        .and_then(finite)
        .unwrap_or_else(|| browser["captured_at_ms"].clone());
    Ok(json!({
        "captured_at_ms": captured_at_ms,
        "session_id": session_id,
        "browser": browser,
        "coord": coord.map(|coord| json!({
            "build": coord
                .get("build")
                .and_then(diagnostic_build)
                .unwrap_or_else(|| json!({ "git_sha": null, "artifact_version": null })),
            "session": coord_session.map(|session| Value::Object(session.clone())),
            "terminal_control": terminal_control,
        })),
        "worker": {
            "worker_fp": worker_fp,
            "status": status,
            "response_ms": response_ms,
            "build": worker_snapshot
                .and_then(|snapshot| snapshot.get("build"))
                .and_then(diagnostic_build),
            "session": worker_session.map(|session| Value::Object(session.clone())),
            "error": worker_error,
        },
    }))
}

/// A JSON object, and nothing else: an array is not a record.
fn record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// A member copied as-is; absent reads as `null`, as v2's `undefined` does
/// once serialized.
fn member(object: &Map<String, Value>, key: &str) -> Value {
    object.get(key).cloned().unwrap_or(Value::Null)
}

/// A finite number, as it arrived.
fn finite(value: &Value) -> Option<Value> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .map(|_| value.clone())
}

/// `diagnosticBuild`: the two build strings of a record, `None` for a
/// non-record.
fn diagnostic_build(value: &Value) -> Option<Value> {
    let build = record(value)?;
    Some(json!({
        "git_sha": string_member(build, "git_sha"),
        "artifact_version": string_member(build, "artifact_version"),
    }))
}
