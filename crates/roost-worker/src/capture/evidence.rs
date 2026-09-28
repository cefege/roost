//! Remote evidence for one CAPTURE: envelope-check the browser and coordinator
//! JSON, and derive from the browser's painted states the exact absolute
//! history rows the worker reads back. Ports `apps/worker/src/diag/
//! terminal-capture-evidence.ts`; called by `super::write` before the freeze.
//! No remote field is trusted for anything but a bounded row range.

use serde_json::{Map, Value};

use roost_protocol::terminal_capture::bundle::TerminalCaptureLayer;
use roost_protocol::terminal_capture::envelope::{EvidenceOwner, check_terminal_capture_envelope};
use roost_protocol::terminal_capture::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode};

/// Painted states a browser section carries; each names DOM history rows and
/// the gaps around them.
const BROWSER_STATE_FIELDS: [&str; 4] = [
    "trigger_state",
    "pre_repair_state",
    "post_repair_state",
    "current_state",
];

/// Disjoint ranges one browser may name. Each can be reported as up to THREE
/// entries (evicted prefix, present body, unavailable suffix) and
/// `worker.history_ranges` is validated against `layer_entries`.
const MAX_EVIDENCE_RANGES: usize = TERMINAL_CAPTURE_LIMITS.layer_entries / 3;

/// JavaScript's largest exact integer: a row index past it is not a row.
const MAX_SAFE_ROW: u64 = (1 << 53) - 1;

/// Absolute history rows, end exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRequest {
    pub start: u64,
    pub end: u64,
}

/// One layer's evidence: its NESTED section and the trigger it shipped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteEvidence {
    pub section: Option<Map<String, Value>>,
    pub trigger: Option<Map<String, Value>>,
}

/// Envelope-check one layer's payload and return the NESTED section, never
/// the envelope. An empty payload is a legitimate absence, not an error.
pub fn parse_remote_evidence(
    json: &str,
    layer: TerminalCaptureLayer,
    owner: &EvidenceOwner<'_>,
) -> Result<RemoteEvidence, TerminalCaptureErrorCode> {
    if json.is_empty() {
        return Ok(RemoteEvidence::default());
    }
    let checked =
        check_terminal_capture_envelope(json, layer, owner).map_err(|refusal| refusal.code)?;
    Ok(RemoteEvidence {
        section: Some(checked.section),
        trigger: checked.trigger,
    })
}

/// The absolute rows the browser evidence named, as coalesced sorted ranges.
/// Empty without browser evidence: the worker then reads its own newest tail.
pub fn history_ranges_from_browser_evidence(
    browser: Option<&Map<String, Value>>,
) -> Vec<HistoryRequest> {
    let Some(browser) = browser else {
        return Vec::new();
    };
    let mut indices = Vec::new();
    let mut ranges = Vec::new();
    for field in BROWSER_STATE_FIELDS {
        let Some(state) = browser.get(field).and_then(Value::as_object) else {
            continue;
        };
        collect_dom_history_indices(state, &mut indices);
        collect_gap_ranges(state, &mut ranges);
    }
    for range in coalesce_indices(indices) {
        if ranges.len() >= MAX_EVIDENCE_RANGES {
            break;
        }
        ranges.push(range);
    }
    ranges.sort_by_key(|range| range.start);
    ranges.truncate(MAX_EVIDENCE_RANGES);
    ranges
}

fn collect_dom_history_indices(state: &Map<String, Value>, indices: &mut Vec<u64>) {
    let Some(rows) = state.get("dom_history").and_then(Value::as_array) else {
        return;
    };
    for row in rows {
        if indices.len() >= TERMINAL_CAPTURE_LIMITS.browser_rows_max {
            return;
        }
        if let Some(index) = row.get("index").and_then(safe_row_number) {
            indices.push(index);
        }
    }
}

fn collect_gap_ranges(state: &Map<String, Value>, ranges: &mut Vec<HistoryRequest>) {
    let Some(gaps) = state.get("gaps").and_then(Value::as_array) else {
        return;
    };
    for gap in gaps {
        if ranges.len() >= MAX_EVIDENCE_RANGES {
            return;
        }
        let (Some(start), Some(end)) = (
            gap.get("start").and_then(safe_row_index),
            gap.get("end").and_then(safe_row_index),
        ) else {
            continue;
        };
        if end <= start {
            continue;
        }
        let capped = end.min(start + TERMINAL_CAPTURE_LIMITS.capture_history_rows as u64);
        ranges.push(HistoryRequest { start, end: capped });
    }
}

/// A JSON number that is an exact non-negative integer (v2 `isSafeInteger`).
fn safe_row_number(value: &Value) -> Option<u64> {
    let number = value.as_number()?;
    let row = match (number.as_u64(), number.as_f64()) {
        (Some(row), _) => row,
        (None, Some(float))
            if float.fract() == 0.0 && (0.0..=MAX_SAFE_ROW as f64).contains(&float) =>
        {
            float as u64
        }
        _ => return None,
    };
    (row <= MAX_SAFE_ROW).then_some(row)
}

/// Absolute row indices are decimal STRINGS on the wire; a number is accepted
/// too. Anything past the safe-integer range is not a row request.
fn safe_row_index(value: &Value) -> Option<u64> {
    let Some(text) = value.as_str() else {
        return safe_row_number(value);
    };
    let digits = text.as_bytes();
    let canonical = !digits.is_empty()
        && digits.len() <= 16
        && digits.iter().all(u8::is_ascii_digit)
        && (digits.len() == 1 || digits[0] != b'0');
    let parsed: u64 = if canonical {
        text.parse().ok()?
    } else {
        return None;
    };
    (parsed <= MAX_SAFE_ROW).then_some(parsed)
}

fn coalesce_indices(mut indices: Vec<u64>) -> Vec<HistoryRequest> {
    let Some(&first) = indices.iter().min() else {
        return Vec::new();
    };
    indices.sort_unstable();
    let mut ranges = Vec::new();
    let (mut start, mut end) = (first, first + 1);
    for index in indices {
        if index <= end {
            end = end.max(index + 1);
            continue;
        }
        ranges.push(HistoryRequest { start, end });
        if ranges.len() >= MAX_EVIDENCE_RANGES {
            return ranges;
        }
        (start, end) = (index, index + 1);
    }
    ranges.push(HistoryRequest { start, end });
    ranges
}
