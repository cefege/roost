//! The shallow envelope check a bridge or the worker runs on one layer's
//! untrusted evidence JSON: size, schema, layer, the capture identity it must
//! belong to, and the layer's section NESTED under a member named for it.
//! Ports `checkTerminalCaptureEnvelope` of `packages/protocol/src/
//! terminal-capture.ts`; the worker's `capture::evidence` calls it. Deep row
//! validation is `super::validate`'s job, not this one's.

use serde_json::{Map, Value};

use super::bundle::{TERMINAL_INCIDENT_SCHEMA, TerminalCaptureLayer};
use super::validate_fields::{CaptureFieldRefusal, refuse};
use super::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode};

/// The capture a payload must belong to. Cross-session and cross-capture
/// evidence is refused before anything forwards or stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceOwner<'a> {
    pub capture_id: &'a str,
    pub recording_id: &'a str,
    pub session_id: &'a str,
}

/// What a well-formed payload carries.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedEvidence {
    /// The layer's own section, unwrapped from the envelope and proved to be a
    /// plain object.
    pub section: Map<String, Value>,
    /// The payload's trigger, when it carries a plain object there. Proved to
    /// be an object and nothing more: the write-side gate judges its members.
    pub trigger: Option<Map<String, Value>>,
}

/// Check one layer's evidence JSON against the capture it claims to belong to.
///
/// The section is NESTED under a member named for its layer and never
/// flattened onto the envelope: a flattened payload validates as an envelope
/// and then fails as a section, silently dropping a whole layer's evidence.
pub fn check_terminal_capture_envelope(
    json: &str,
    layer: TerminalCaptureLayer,
    owner: &EvidenceOwner<'_>,
) -> Result<CheckedEvidence, CaptureFieldRefusal> {
    use TerminalCaptureErrorCode::{EvidenceMalformed, EvidenceTooLarge, PermissionDenied};
    if json.len() > TERMINAL_CAPTURE_LIMITS.browser_evidence_bytes {
        return Err(refuse(EvidenceTooLarge, "browser_evidence_json"));
    }
    let Ok(Value::Object(mut record)) = serde_json::from_str::<Value>(json) else {
        return Err(refuse(EvidenceMalformed, "browser_evidence_json"));
    };
    if record.get("schema").and_then(Value::as_str) != Some(TERMINAL_INCIDENT_SCHEMA) {
        return Err(refuse(EvidenceMalformed, "schema"));
    }
    if record.get("layer").and_then(Value::as_str) != Some(layer.as_str()) {
        return Err(refuse(EvidenceMalformed, "layer"));
    }
    let identity = [
        ("capture_id", owner.capture_id),
        ("recording_id", owner.recording_id),
        ("session_id", owner.session_id),
    ];
    for (field, expected) in identity {
        if record.get(field).and_then(Value::as_str) != Some(expected) {
            return Err(refuse(PermissionDenied, field));
        }
    }
    let Some(Value::Object(section)) = record.remove(layer.as_str()) else {
        return Err(refuse(EvidenceMalformed, layer.as_str()));
    };
    let trigger = match record.remove("trigger") {
        Some(Value::Object(trigger)) => Some(trigger),
        _ => None,
    };
    Ok(CheckedEvidence { section, trigger })
}
