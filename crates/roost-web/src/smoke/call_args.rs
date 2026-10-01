//! The argument readers behind `call::parse_call`: typed reads of the JSON
//! argument list with the refusal each wrong type answers, and the structured
//! options of `waitForPaintedCursor`, `beginTerminalTiming` and
//! `runRenderStress`. Native. Ports the argument contracts of
//! `apps/web/src/smoke/smokeTypes.ts:118-287` and
//! `apps/web/src/smoke/smokeHarness.ts:326-333`.

use serde_json::{Map, Value};

use super::call::{RenderStressOptions, TimingKind};

pub(super) struct Args<'a> {
    pub(super) method: &'static str,
    pub(super) args: &'a [Value],
}

impl Args<'_> {
    pub(super) fn at(&self, index: usize) -> &Value {
        self.args.get(index).unwrap_or(&Value::Null)
    }

    pub(super) fn wrong(&self, index: usize, expected: &str) -> String {
        format!(
            "__smoke.{}: argument {} must be {expected}",
            self.method,
            index + 1
        )
    }

    pub(super) fn string(&self, index: usize) -> Result<String, String> {
        self.at(index)
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| self.wrong(index, "a string"))
    }

    /// A boolean flag, defaulting to `true` when the caller passed nothing.
    ///
    /// The default is what the two visibility pins need: both are
    /// `forceVisible(on)` / `forceHidden(on)` in v2, and a spec that calls
    /// `forceVisible()` plainly means "pin me visible". Reading the argument is
    /// what makes `forceVisible(false)` mean "release the pin", which is the
    /// only way a spec turns it off.
    pub(super) fn flag(&self, index: usize) -> Result<bool, String> {
        match self.at(index) {
            Value::Null => Ok(true),
            value => value
                .as_bool()
                .ok_or_else(|| self.wrong(index, "a boolean")),
        }
    }

    pub(super) fn optional_string(&self, index: usize) -> Result<Option<String>, String> {
        match self.at(index) {
            Value::Null => Ok(None),
            Value::String(text) => Ok(Some(text.clone())),
            _ => Err(self.wrong(index, "a string or omitted")),
        }
    }

    pub(super) fn optional_number(&self, index: usize) -> Result<Option<f64>, String> {
        match self.at(index) {
            Value::Null => Ok(None),
            value => value
                .as_f64()
                .map(Some)
                .ok_or_else(|| self.wrong(index, "a number")),
        }
    }

    pub(super) fn number_or(&self, index: usize, default: f64) -> Result<f64, String> {
        Ok(self.optional_number(index)?.unwrap_or(default))
    }

    pub(super) fn count(&self, index: usize) -> Result<u64, String> {
        self.at(index)
            .as_u64()
            .ok_or_else(|| self.wrong(index, "a non-negative integer"))
    }

    pub(super) fn row(&self, index: usize) -> Result<u32, String> {
        u32::try_from(self.count(index)?).map_err(|_| self.wrong(index, "a row index"))
    }

    pub(super) fn object_or_null(
        &self,
        index: usize,
    ) -> Result<Option<&Map<String, Value>>, String> {
        match self.at(index) {
            Value::Null => Ok(None),
            Value::Object(fields) => Ok(Some(fields)),
            _ => Err(self.wrong(index, "an object or omitted")),
        }
    }
}

pub(super) fn timing_kind(name: &str) -> Result<TimingKind, String> {
    Ok(match name {
        "trusted_key" => TimingKind::TrustedKey,
        "reveal" => TimingKind::Reveal,
        "resize" => TimingKind::Resize,
        "optimistic" => TimingKind::Optimistic,
        other => return Err(format!("unknown terminal timing kind: {other}")),
    })
}

/// A cursor coordinate an expectation may pin: absent, or a non-negative safe
/// integer — anything else is v2's `invalid expected cursor coordinates`.
pub(super) fn cursor_coordinate(
    expected: Option<&Map<String, Value>>,
    field: &str,
) -> Result<Option<u32>, String> {
    let Some(value) = expected.and_then(|fields| fields.get(field)) else {
        return Ok(None);
    };
    match value.as_u64().and_then(|number| u32::try_from(number).ok()) {
        Some(coordinate) => Ok(Some(coordinate)),
        None => Err(format!(
            "invalid expected cursor coordinates: {}",
            Value::Object(expected.cloned().unwrap_or_default())
        )),
    }
}

pub(super) fn optional_field_string(
    method: &str,
    fields: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, String> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("__smoke.{method}: {field} must be a string")),
    }
}

pub(super) fn render_stress_options(arg: &Args<'_>) -> Result<RenderStressOptions, String> {
    let fields = arg
        .object_or_null(0)?
        .ok_or_else(|| arg.wrong(0, "an options object"))?;
    let text = |field: &str| {
        fields
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("__smoke.runRenderStress: {field} must be a string"))
    };
    let main_screen = match text("screen")?.as_str() {
        "main" => true,
        "alt" => false,
        other => return Err(format!("__smoke.runRenderStress: unknown screen {other}")),
    };
    let iterations = fields
        .get("iterations")
        .and_then(Value::as_u64)
        .and_then(|count| u32::try_from(count).ok())
        .ok_or_else(|| {
            "__smoke.runRenderStress: iterations must be a non-negative integer".to_owned()
        })?;
    Ok(RenderStressOptions {
        session_id: text("sessionId")?,
        prefix: text("prefix")?,
        main_screen,
        iterations,
    })
}
