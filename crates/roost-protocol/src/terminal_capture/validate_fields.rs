//! Field-level checks shared by the incident-bundle validator: JS number
//! semantics over `serde_json::Value`, decimal uint64 offsets, stream
//! identities, frames and rows. Ports `packages/protocol/src/
//! terminal-capture-fields.ts`; called by `super::validate` and
//! `super::validate_layers`. Every rejection is a fixed code plus a field PATH,
//! never the value, because the values here are terminal content.

use serde_json::{Map, Value};

use super::TerminalCaptureErrorCode;

/// A refused bundle or payload: which code, and WHERE — never what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFieldRefusal {
    pub code: TerminalCaptureErrorCode,
    pub field: String,
}

pub(crate) type FieldCheck = Result<(), CaptureFieldRefusal>;

/// JavaScript's `Number.MAX_SAFE_INTEGER`: the bound v2's `isSafeInteger`
/// checks every count against.
const MAX_SAFE_INTEGER: i64 = (1_i64 << 53) - 1;
const MAX_DIMENSION: i64 = 4096;

pub(crate) fn refuse(
    code: TerminalCaptureErrorCode,
    field: impl Into<String>,
) -> CaptureFieldRefusal {
    CaptureFieldRefusal {
        code,
        field: field.into(),
    }
}

pub(crate) fn fail(code: TerminalCaptureErrorCode, field: impl Into<String>) -> FieldCheck {
    Err(refuse(code, field))
}

/// v2 `asRecord`: a plain object, or nothing.
pub(crate) fn as_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// v2 `Number.isSafeInteger`: an integral JSON number within ±(2^53 − 1).
pub(crate) fn safe_integer(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_number()?;
    let integral = if let Some(signed) = number.as_i64() {
        signed
    } else if number.is_u64() {
        return None;
    } else {
        let float = number.as_f64()?;
        if !float.is_finite() || float.fract() != 0.0 || float.abs() > MAX_SAFE_INTEGER as f64 {
            return None;
        }
        float as i64
    };
    (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER)
        .contains(&integral)
        .then_some(integral)
}

pub(crate) fn is_count(value: Option<&Value>) -> bool {
    safe_integer(value).is_some_and(|number| number >= 0)
}

pub(crate) fn is_epoch_ms(value: Option<&Value>) -> bool {
    safe_integer(value).is_some_and(|number| number > 0)
}

pub(crate) fn is_dimension(value: Option<&Value>) -> bool {
    safe_integer(value).is_some_and(|number| (1..=MAX_DIMENSION).contains(&number))
}

/// A present `null` or a string. A MISSING member is neither, as in v2, where
/// `undefined` fails both halves.
pub(crate) fn is_nullable_string(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Null | Value::String(_)))
}

/// True for a decimal uint64 string — the only exact JSON form of a raw byte
/// offset or a stream sequence. No sign, no leading zero, no exponent.
pub fn is_decimal_uint64(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(is_decimal_uint64_text)
}

/// The same rule over a string already in hand.
pub fn is_decimal_uint64_text(text: &str) -> bool {
    let digits = text.as_bytes();
    if digits.is_empty() || digits.len() > 20 || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return false;
    }
    text.parse::<u64>().is_ok()
}

/// The value of an already-validated decimal uint64 member.
pub(crate) fn decimal_value(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

pub(crate) fn validate_stream_identity(value: Option<&Value>, path: &str) -> FieldCheck {
    use TerminalCaptureErrorCode::{EvidenceMalformed, InvalidArgument};
    let Some(stream) = as_record(value) else {
        return fail(EvidenceMalformed, path);
    };
    if !stream
        .get("stream_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return fail(InvalidArgument, format!("{path}.stream_id"));
    }
    if !stream
        .get("grid_epoch")
        .and_then(Value::as_str)
        .is_some_and(|epoch| !epoch.is_empty())
    {
        return fail(InvalidArgument, format!("{path}.grid_epoch"));
    }
    if !is_decimal_uint64(stream.get("seq")) {
        return fail(InvalidArgument, format!("{path}.seq"));
    }
    let base_seq = stream.get("base_seq");
    if base_seq != Some(&Value::Null) && !is_decimal_uint64(base_seq) {
        return fail(InvalidArgument, format!("{path}.base_seq"));
    }
    if !is_dimension(stream.get("cols")) || !is_dimension(stream.get("rows")) {
        return fail(InvalidArgument, format!("{path}.geometry"));
    }
    Ok(())
}

/// Structural check of a frame as it survived JSON: dimensions in range, a
/// dense index-ordered viewport for a full, and every span claiming a column.
pub(crate) fn validate_frame(value: Option<&Value>, path: &str) -> FieldCheck {
    use TerminalCaptureErrorCode::{EvidenceMalformed, InvalidArgument};
    let Some(frame) = as_record(value) else {
        return fail(EvidenceMalformed, path);
    };
    if !is_dimension(frame.get("cols")) || !is_dimension(frame.get("rows")) {
        return fail(InvalidArgument, format!("{path}.geometry"));
    }
    let Some(full) = frame.get("full").and_then(Value::as_bool) else {
        return fail(InvalidArgument, format!("{path}.full"));
    };
    if !frame.get("streamId").is_some_and(Value::is_string)
        || !frame.get("gridEpoch").is_some_and(Value::is_string)
    {
        return fail(InvalidArgument, format!("{path}.identity"));
    }
    if !is_count(frame.get("seq")) || !is_count(frame.get("baseSeq")) {
        return fail(InvalidArgument, format!("{path}.seq"));
    }
    if !is_count(frame.get("scrollbackTotal")) || !is_count(frame.get("sbBase")) {
        return fail(InvalidArgument, format!("{path}.scrollback"));
    }
    for name in ["viewportRows", "scrollbackRows", "scrollbackAppend"] {
        let Some(rows) = frame.get(name).and_then(Value::as_array) else {
            return fail(InvalidArgument, format!("{path}.{name}"));
        };
        for (position, row) in rows.iter().enumerate() {
            validate_row(Some(row), &format!("{path}.{name}[{position}]"))?;
        }
    }
    if full {
        let viewport = frame
            .get("viewportRows")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if safe_integer(frame.get("rows")) != Some(viewport.len() as i64) {
            return fail(InvalidArgument, format!("{path}.viewportRows"));
        }
        for (position, row) in viewport.iter().enumerate() {
            if safe_integer(row.get("index")) != Some(position as i64) {
                return fail(
                    InvalidArgument,
                    format!("{path}.viewportRows[{position}].index"),
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_row(value: Option<&Value>, path: &str) -> FieldCheck {
    use TerminalCaptureErrorCode::{EvidenceMalformed, InvalidArgument};
    let Some(row) = as_record(value) else {
        return fail(EvidenceMalformed, path);
    };
    if !is_count(row.get("index")) {
        return fail(InvalidArgument, format!("{path}.index"));
    }
    let Some(spans) = row.get("spans").and_then(Value::as_array) else {
        return fail(InvalidArgument, format!("{path}.spans"));
    };
    for (position, span) in spans.iter().enumerate() {
        let span_path = format!("{path}.spans[{position}]");
        let Some(span) = span
            .as_object()
            .filter(|span| span.get("text").is_some_and(Value::is_string))
        else {
            return fail(EvidenceMalformed, span_path);
        };
        if !safe_integer(span.get("columns")).is_some_and(|columns| columns >= 1) {
            return fail(InvalidArgument, format!("{span_path}.columns"));
        }
        if !is_count(span.get("fg")) || !is_count(span.get("bg")) || !is_count(span.get("flags")) {
            return fail(InvalidArgument, format!("{span_path}.style"));
        }
    }
    Ok(())
}
