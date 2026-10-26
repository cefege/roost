//! Pinned-source trace loading and structural JSON comparison.
//! Used by the native oracle runner and external trace comparisons.
//! Numeric encoding differences never permit losing integer identity.
use serde::Deserialize;
use serde_json::Value;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::Path;

const UPSTREAM_REPOSITORY: &str = "https://github.com/earendil-works/pi";
const UPSTREAM_COMMIT: &str = "d5b3de22d0e24a3e9c38a9410d3329b099717283";
const UPSTREAM_PACKAGE: &str = "packages/durable";

#[derive(Debug, Deserialize)]
struct SourceIdentity {
    repository: String,
    commit: String,
    package: String,
}

#[derive(Debug, Deserialize)]
struct FixtureEnvelope {
    source: SourceIdentity,
    normalization: String,
    trace: Value,
    sha256: String,
}

/// A generated upstream trace and its pinned source identity.
#[derive(Debug)]
pub struct TraceFixture {
    pub name: String,
    pub trace: Value,
    pub normalization: String,
    pub sha256: String,
}

/// Why the fixture could not be loaded or matched against the native trace.
#[derive(Debug)]
pub enum FixtureError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(String),
    Mismatch {
        fixture: String,
        expected: Value,
        actual: Value,
    },
}

impl Display for FixtureError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "could not read fixture or trace: {error}"),
            Self::Json(error) => write!(formatter, "invalid fixture or trace JSON: {error}"),
            Self::Invalid(message) => formatter.write_str(message),
            Self::Mismatch {
                fixture,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "native trace differs from {fixture}: expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl Error for FixtureError {}

impl From<std::io::Error> for FixtureError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for FixtureError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Load a fixture, refusing a different source pin or a name inconsistent with its file.
pub fn load_fixture(path: impl AsRef<Path>) -> Result<TraceFixture, FixtureError> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path)?;
    let envelope: FixtureEnvelope = serde_json::from_str(&contents)?;
    if envelope.source.repository != UPSTREAM_REPOSITORY
        || envelope.source.commit != UPSTREAM_COMMIT
        || envelope.source.package != UPSTREAM_PACKAGE
    {
        return Err(FixtureError::Invalid(format!(
            "{} is not generated from the pinned Pi Durable source",
            path.display()
        )));
    }
    let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return Err(FixtureError::Invalid(format!(
            "fixture path has no UTF-8 file name: {}",
            path.display()
        )));
    };
    if envelope.trace.get("name").and_then(Value::as_str) != Some(name) {
        return Err(FixtureError::Invalid(format!(
            "fixture trace name does not match {name}"
        )));
    }
    if envelope.sha256.len() != 64 || !envelope.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(FixtureError::Invalid(format!(
            "fixture {name} has an invalid trace SHA-256"
        )));
    }
    Ok(TraceFixture {
        name: name.to_owned(),
        trace: envelope.trace,
        normalization: envelope.normalization,
        sha256: envelope.sha256,
    })
}

/// Compare a native adapter trace with the already-normalized upstream trace.
pub fn compare_trace(fixture: &TraceFixture, actual: &Value) -> Result<(), FixtureError> {
    if json_values_equal(&fixture.trace, actual) {
        Ok(())
    } else {
        Err(FixtureError::Mismatch {
            fixture: fixture.name.clone(),
            expected: fixture.trace.clone(),
            actual: actual.clone(),
        })
    }
}

/// Read a native trace JSON document and compare it to a pinned upstream fixture.
pub fn compare_trace_file(
    fixture_path: impl AsRef<Path>,
    actual_path: impl AsRef<Path>,
) -> Result<(), FixtureError> {
    let fixture = load_fixture(fixture_path)?;
    let actual = serde_json::from_str(&fs::read_to_string(actual_path)?)?;
    compare_trace(&fixture, &actual)
}

fn json_values_equal(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Number(expected), Value::Number(actual)) => {
            expected == actual
                || exact_number(expected)
                    .is_some_and(|expected| exact_number(actual) == Some(expected))
        }
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| json_values_equal(expected, actual))
        }
        (Value::Object(expected), Value::Object(actual)) => {
            expected.len() == actual.len()
                && expected.iter().all(|(key, expected)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| json_values_equal(expected, actual))
                })
        }
        _ => expected == actual,
    }
}

fn exact_number(number: &serde_json::Number) -> Option<f64> {
    let value = number.as_f64()?;
    if number
        .as_i64()
        .is_some_and(|integer| value as i128 != i128::from(integer))
        || number
            .as_u64()
            .is_some_and(|integer| value as i128 != i128::from(integer))
    {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::json_values_equal;
    use serde_json::json;

    #[test]
    fn numeric_encoding_preserves_identity_and_array_order() {
        assert!(json_values_equal(
            &json!({"id": 7, "cost": 0}),
            &json!({"id": 7.0, "cost": 0.0})
        ));
        assert!(!json_values_equal(
            &json!(9_007_199_254_740_993_u64),
            &json!(9_007_199_254_740_992.0)
        ));
        assert!(!json_values_equal(&json!([1, 2]), &json!([2.0, 1.0])));
        assert!(!json_values_equal(&json!({"value": null}), &json!({})));
        assert!(!json_values_equal(&json!(false), &json!(0)));
    }
}
