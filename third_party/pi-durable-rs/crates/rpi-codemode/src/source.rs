//! Codemode source format (port of `packages/codemode/src/source.ts` @
//! abe508e1b): JavaScript, optionally preceded by one options line.
//!
//! ```js
//! // @options: {"max_output_tokens": 2000, "timeout_ms": 30000}
//! const text = await tools.read({ path: "package.json" });
//! text(JSON.parse(text).name);
//! ```

use serde_json::Value;

/// `CODEMODE_OPTIONS_PREFIX` (source.ts:11).
pub const CODEMODE_OPTIONS_PREFIX: &str = "// @options:";

/// `CODEMODE_SOURCE_GRAMMAR` (source.ts:18-30). Kept verbatim for the
/// constrained-sampling surface; rpi's providers do not currently consume a
/// Lark grammar (see V16-07 §8-4 `[N/A]`).
pub const CODEMODE_SOURCE_GRAMMAR: &str = r#"
start: options_source | plain_source
options_source: OPTIONS_LINE NEWLINE SOURCE
plain_source: SOURCE

OPTIONS_LINE: /[ \t]*\/\/ @options:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

const SUPPORTED_FIELDS: [&str; 2] = ["max_output_tokens", "timeout_ms"];
const SUPPORTED_FIELDS_TEXT: &str = "`max_output_tokens` and `timeout_ms`";
/// Largest delay `setTimeout` supports, which bounds `timeout_ms`
/// (source.ts:15-16).
const MAX_TIMEOUT_MS: u64 = 2_147_483_647;
/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `CodemodeSourceOptions` (source.ts:32-37).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodemodeSourceOptions {
    /// Token budget for the script's output.
    pub max_output_tokens: Option<u64>,
    /// Hard deadline for the whole script in milliseconds, including tool
    /// calls.
    pub timeout_ms: Option<u64>,
}

/// `ParsedCodemodeSource` (source.ts:39-43).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCodemodeSource {
    /// The script with the options line replaced by an empty line, so line
    /// numbers are unchanged.
    pub code: String,
    pub options: CodemodeSourceOptions,
}

/// `CodemodeSourceError` (source.ts:45-50).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CodemodeSourceError {
    pub message: String,
}

impl CodemodeSourceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// `isSafeInteger` + `value >= 0` (source.ts:52-54).
fn is_safe_non_negative_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if number.is_finite() && number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number) {
        Some(number as u64)
    } else {
        None
    }
}

fn parse_options(directive: &str) -> Result<CodemodeSourceOptions, CodemodeSourceError> {
    if directive.is_empty() {
        return Err(CodemodeSourceError::new(format!(
            "@options must be a JSON object with supported fields {SUPPORTED_FIELDS_TEXT}"
        )));
    }
    let value: Value = serde_json::from_str(directive).map_err(|error| {
        CodemodeSourceError::new(format!(
            "@options must be valid JSON with supported fields {SUPPORTED_FIELDS_TEXT}: {error}"
        ))
    })?;
    let Some(fields) = value.as_object() else {
        return Err(CodemodeSourceError::new(format!(
            "@options must be a JSON object with supported fields {SUPPORTED_FIELDS_TEXT}"
        )));
    };
    for key in fields.keys() {
        if !SUPPORTED_FIELDS.contains(&key.as_str()) {
            return Err(CodemodeSourceError::new(format!(
                "@options only supports {SUPPORTED_FIELDS_TEXT}; got `{key}`"
            )));
        }
    }
    let mut options = CodemodeSourceOptions::default();
    if let Some(value) = fields.get("max_output_tokens") {
        options.max_output_tokens = Some(is_safe_non_negative_integer(value).ok_or_else(|| {
            CodemodeSourceError::new(
                "@options field `max_output_tokens` must be a non-negative safe integer",
            )
        })?);
    }
    if let Some(value) = fields.get("timeout_ms") {
        let timeout = is_safe_non_negative_integer(value).ok_or_else(|| {
            CodemodeSourceError::new(format!(
                "@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}"
            ))
        })?;
        if timeout == 0 || timeout > MAX_TIMEOUT_MS {
            return Err(CodemodeSourceError::new(format!(
                "@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}"
            )));
        }
        options.timeout_ms = Some(timeout);
    }
    Ok(options)
}

/// Split an optional first-line `// @options: {...}` from the script. Errors
/// for empty input and invalid options.
pub fn parse_codemode_source(input: &str) -> Result<ParsedCodemodeSource, CodemodeSourceError> {
    if input.trim().is_empty() {
        return Err(CodemodeSourceError::new(
            "Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {\"max_output_tokens\": 1000}`.",
        ));
    }
    let newline = input.find('\n');
    let first_line = match newline {
        Some(index) => &input[..index],
        None => input,
    };
    let first_line = first_line.strip_suffix('\r').unwrap_or(first_line);
    let trimmed = first_line.trim_start();
    let Some(directive) = trimmed.strip_prefix(CODEMODE_OPTIONS_PREFIX) else {
        return Ok(ParsedCodemodeSource {
            code: input.to_owned(),
            options: CodemodeSourceOptions::default(),
        });
    };
    let code = match newline {
        Some(index) => &input[index..],
        None => "",
    };
    if code.trim().is_empty() {
        return Err(CodemodeSourceError::new(
            "The @options line must be followed by JavaScript source on subsequent lines",
        ));
    }
    Ok(ParsedCodemodeSource {
        code: code.to_owned(),
        options: parse_options(directive.trim())?,
    })
}

#[cfg(test)]
mod tests {
    //! Ports `packages/codemode/test/source.test.ts` @ abe508e1b.

    use super::*;

    #[test]
    fn returns_plain_code_unchanged() {
        let parsed = parse_codemode_source("text('hi')").expect("plain code");
        assert_eq!(parsed.code, "text('hi')");
        assert_eq!(parsed.options, CodemodeSourceOptions::default());
        let parsed = parse_codemode_source("// just a comment\nreturn 1").expect("comment");
        assert_eq!(parsed.code, "// just a comment\nreturn 1");
        assert_eq!(parsed.options, CodemodeSourceOptions::default());
    }

    #[test]
    fn parses_the_options_line_and_keeps_line_numbers() {
        let parsed =
            parse_codemode_source("// @options: {\"timeout_ms\": 10}\nconst a = 1;\ntext(a)")
                .expect("options");
        assert_eq!(parsed.code, "\nconst a = 1;\ntext(a)");
        assert_eq!(
            parsed.options,
            CodemodeSourceOptions {
                max_output_tokens: None,
                timeout_ms: Some(10)
            }
        );
        let parsed = parse_codemode_source(
            "  // @options:{\"max_output_tokens\":0,\"timeout_ms\":1500}\r\ntext(1)",
        )
        .expect("crlf options");
        assert_eq!(
            parsed.options,
            CodemodeSourceOptions {
                max_output_tokens: Some(0),
                timeout_ms: Some(1500)
            }
        );
        let parsed = parse_codemode_source("// @options: {}\ntext(1)").expect("empty object");
        assert_eq!(parsed.code, "\ntext(1)");
        assert_eq!(parsed.options, CodemodeSourceOptions::default());
    }

    #[test]
    fn only_treats_the_first_line_as_an_options_line() {
        let input = "text(1)\n// @options: {\"timeout_ms\": 1}";
        let parsed = parse_codemode_source(input).expect("second line");
        assert_eq!(parsed.code, input);
        assert_eq!(parsed.options, CodemodeSourceOptions::default());
        let parsed = parse_codemode_source("// @optionsx {}\ntext(1)").expect("typo");
        assert_eq!(parsed.options, CodemodeSourceOptions::default());
    }

    #[test]
    fn rejects_empty_input_and_invalid_options() {
        let cases: [(&str, &str); 10] = [
            ("", "Expected JavaScript source text (non-empty)"),
            ("  \n", "Expected JavaScript source text (non-empty)"),
            (
                "// @options:\ntext(1)",
                "@options must be a JSON object with supported fields",
            ),
            (
                "// @options: {timeout_ms: 1}\ntext(1)",
                "@options must be valid JSON with supported fields",
            ),
            (
                "// @options: [1]\ntext(1)",
                "@options must be a JSON object with supported fields",
            ),
            (
                "// @options: {\"yield\": 1}\ntext(1)",
                "@options only supports `max_output_tokens` and `timeout_ms`; got `yield`",
            ),
            (
                "// @options: {\"max_output_tokens\": 1.5}\ntext(1)",
                "@options field `max_output_tokens` must be a non-negative safe integer",
            ),
            (
                "// @options: {\"timeout_ms\": 0}\ntext(1)",
                "@options field `timeout_ms` must be a positive integer",
            ),
            (
                "// @options: {\"timeout_ms\": 1}",
                "The @options line must be followed by JavaScript source on subsequent lines",
            ),
            (
                "// @options: {\"timeout_ms\": 1}\n  \n",
                "The @options line must be followed by JavaScript source on subsequent lines",
            ),
        ];
        for (input, message) in cases {
            let error = parse_codemode_source(input).expect_err(input);
            assert!(
                error.message.contains(message),
                "input {input:?}: {} does not contain {message:?}",
                error.message
            );
        }
    }
}
