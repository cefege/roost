//! Turns the frozen layer sections into ONE owner-only
//! `terminal-incident-<capture-id>.json.gz`: fit the uncompressed budget by
//! dropping whole sections and record arrays (never by truncating a string),
//! run the shared bundle validator as a write-side gate, gzip, and hand the
//! bytes to `super::storage`. Ports `apps/worker/src/diag/
//! terminal-capture-bundle-writer.ts`; called by `super::write`, off the PTY path.

use std::sync::Arc;

use async_compression::tokio::write::GzipEncoder;
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt as _;

use roost_protocol::terminal_capture::bundle::{
    TERMINAL_INCIDENT_SCHEMA, TerminalCaptureCoverage as Coverage, TerminalCaptureCoverageReport,
    TerminalCoverageReason as Reason, TerminalWorkerSection,
};
use roost_protocol::terminal_capture::validate::validate_terminal_incident_bundle;
use roost_protocol::terminal_capture::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode};

use super::storage::{CaptureStorage, StoredCapture};

/// The worker evidence arrays a trim drops, largest and least attributive
/// first: raw bytes replay nothing without a complete prefix, while the
/// emissions and the trigger's own samples are the attribution itself.
const TRIM_ORDER: [&str; 5] = [
    "raw",
    "core_scrollback_tail",
    "history_rows",
    "core_samples",
    "emissions",
];

/// Everything one bundle is written from.
#[derive(Debug, Clone)]
pub struct IncidentBundleInput {
    pub capture_id: String,
    pub recording_id: String,
    pub session_id: String,
    pub written_at_ms: u64,
    /// Preferred trigger. May be PEER-authored, so it can still fail the gate.
    pub trigger: Value,
    /// Worker-owned trigger, always structurally valid: the remote-drop retry
    /// falls back to it, because a peer trigger belongs to the payload that
    /// just failed validation.
    pub worker_trigger: Value,
    pub coverage: TerminalCaptureCoverageReport,
    pub worker: TerminalWorkerSection,
    pub coordinator: Option<Map<String, Value>>,
    /// Already envelope-checked browser section.
    pub browser: Option<Map<String, Value>>,
}

/// One written bundle, and whether a trim made it partial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenBundle {
    pub stored: StoredCapture,
    pub trimmed: bool,
}

pub async fn write_terminal_incident_bundle(
    storage: &Arc<CaptureStorage>,
    input: IncidentBundleInput,
) -> Result<WrittenBundle, TerminalCaptureErrorCode> {
    // Yield once so compression and the disk write never share a turn with the
    // PTY read that produced the evidence.
    tokio::task::yield_now().await;
    let layers = Layers {
        worker: worker_json(&input.worker)?,
        coordinator: input.coordinator.clone(),
        browser: input.browser.clone(),
    };
    let mut fitted = fit_bundle_budget(&input, &input.trigger, layers);
    let mut validation = validate_terminal_incident_bundle(&fitted.bundle);
    if let Err(refusal) = &validation
        && (input.coordinator.is_some() || input.browser.is_some())
    {
        // Everything the peer authored — both sections AND the trigger it
        // shipped with them — goes together: keeping the peer trigger would
        // fail the retry identically and cost the worker its whole section.
        tracing::warn!(
            capture_id = %input.capture_id,
            code = ?refusal.code,
            field = %refusal.field,
            "diag.terminal_capture_remote_section_dropped"
        );
        let mut worker = worker_json(&input.worker)?;
        push_omission(&mut worker, remote_section_omission(&refusal.field));
        let local = Layers {
            worker,
            coordinator: None,
            browser: None,
        };
        fitted = fit_bundle_budget(&input, &input.worker_trigger, local);
        validation = validate_terminal_incident_bundle(&fitted.bundle);
    }
    if let Err(refusal) = validation {
        tracing::error!(
            capture_id = %input.capture_id,
            code = ?refusal.code,
            field = %refusal.field,
            "diag.terminal_capture_invalid_bundle"
        );
        return Err(TerminalCaptureErrorCode::Internal);
    }
    let payload = gzip(fitted.json.as_bytes())
        .await
        .map_err(|_| TerminalCaptureErrorCode::StorageFailed)?;
    let writer = Arc::clone(storage);
    let capture_id = input.capture_id.clone();
    let stored = tokio::task::spawn_blocking(move || {
        writer.write_terminal_incident_file(&capture_id, &payload)
    })
    .await
    .map_err(|_| TerminalCaptureErrorCode::StorageFailed)??;
    Ok(WrittenBundle {
        stored,
        trimmed: fitted.trimmed,
    })
}

/// The worker section as JSON; the rare remote-drop retry re-serializes it
/// rather than every capture paying for a clone.
fn worker_json(worker: &TerminalWorkerSection) -> Result<Value, TerminalCaptureErrorCode> {
    serde_json::to_value(worker).map_err(|_| TerminalCaptureErrorCode::Internal)
}

async fn gzip(json: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzipEncoder::new(Vec::with_capacity(json.len() / 4));
    encoder.write_all(json).await?;
    encoder.shutdown().await?;
    Ok(encoder.into_inner())
}

/// The three layer sections as JSON, the worker's always present.
#[derive(Debug)]
struct Layers {
    worker: Value,
    coordinator: Option<Map<String, Value>>,
    browser: Option<Map<String, Value>>,
}

#[derive(Debug)]
struct FittedBundle {
    bundle: Value,
    json: String,
    trimmed: bool,
}

/// Drop whole sections and whole record arrays until the UNCOMPRESSED JSON
/// fits. Each drop is named in the worker's omissions and downgrades coverage:
/// a trimmed export is partial, never complete. The bundle is trimmed IN
/// PLACE, so no section is copied to measure it.
fn fit_bundle_budget(input: &IncidentBundleInput, trigger: &Value, layers: Layers) -> FittedBundle {
    let mut bundle = json!({
        "schema": TERMINAL_INCIDENT_SCHEMA,
        "capture_id": input.capture_id,
        "recording_id": input.recording_id,
        "session_id": input.session_id,
        "written_at_ms": input.written_at_ms,
        "trigger": trigger,
        "coverage": input.coverage,
        "browser": layers.browser,
        "coordinator": layers.coordinator,
        "worker": layers.worker,
    });
    let mut json = bundle.to_string();
    if json.len() <= TERMINAL_CAPTURE_LIMITS.bundle_bytes {
        return FittedBundle {
            bundle,
            json,
            trimmed: false,
        };
    }
    bundle["coverage"] = json!(downgrade_coverage(&input.coverage));
    // The worker's evidence arrays first, then its raw tail, then the remote
    // sections: without those the bundle still attributes the worker's layer.
    let arrays = TRIM_ORDER.iter().map(|field| format!("worker.{field}"));
    let trims = arrays.chain(["worker.byte_capture", "coordinator", "browser"].map(str::to_owned));
    for name in trims {
        let Some(dropped) = take_for_trim(&mut bundle, &name) else {
            continue;
        };
        push_omission(&mut bundle["worker"], trim_omission(&name, dropped));
        json = bundle.to_string();
        if json.len() <= TERMINAL_CAPTURE_LIMITS.bundle_bytes {
            break;
        }
    }
    FittedBundle {
        bundle,
        json,
        trimmed: true,
    }
}

/// Empty one trimmable member and say how many entries it held, or `None`
/// when it held nothing to drop.
fn take_for_trim(bundle: &mut Value, name: &str) -> Option<usize> {
    let (holder, field) = match name.strip_prefix("worker.") {
        Some(field) => (&mut bundle["worker"], field),
        None => (bundle, name),
    };
    let slot = holder.get_mut(field)?;
    let (dropped, emptied) = match slot {
        Value::Array(entries) if !entries.is_empty() => (entries.len(), Value::Array(Vec::new())),
        Value::Object(_) => (1, Value::Null),
        _ => return None,
    };
    *slot = emptied;
    Some(dropped)
}

fn push_omission(worker: &mut Value, omission: Value) {
    if let Some(omissions) = worker.get_mut("omissions").and_then(Value::as_array_mut) {
        omissions.push(omission);
    }
}

fn downgrade_coverage(coverage: &TerminalCaptureCoverageReport) -> TerminalCaptureCoverageReport {
    let downgrade = |axis: Coverage| {
        if axis == Coverage::Unavailable {
            Coverage::Unavailable
        } else {
            Coverage::Partial
        }
    };
    let with_trim = |reasons: &[Reason]| {
        let mut kept: Vec<Reason> = reasons
            .iter()
            .copied()
            .filter(|reason| *reason != Reason::Complete)
            .collect();
        kept.push(Reason::EvidenceTrimmed);
        kept
    };
    TerminalCaptureCoverageReport {
        cell_replay: downgrade(coverage.cell_replay),
        cell_replay_reasons: with_trim(&coverage.cell_replay_reasons),
        core_replay: downgrade(coverage.core_replay),
        core_replay_reasons: with_trim(&coverage.core_replay_reasons),
        core_comparison: downgrade(coverage.core_comparison),
        core_comparison_reasons: with_trim(&coverage.core_comparison_reasons),
    }
}

fn trim_omission(name: &str, dropped_count: usize) -> Value {
    let kind = if name == "coordinator" || name == "browser" {
        "section"
    } else {
        "records"
    };
    json!({
        "kind": kind,
        "name": name,
        "reason": "evidence_trimmed",
        "dropped_count": dropped_count,
        "dropped_bytes": 0,
        "range": null,
    })
}

/// The remote layers arrived but did not survive validation. `field` is a
/// validator field PATH, never a value, so it carries no terminal content.
fn remote_section_omission(field: &str) -> Value {
    json!({
        "kind": "section",
        "name": format!("remote:{field}"),
        "reason": "layer_unavailable",
        "dropped_count": 1,
        "dropped_bytes": 0,
        "range": null,
    })
}
