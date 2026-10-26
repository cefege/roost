//! First-line options parser for durable code-mode scripts.
//!
//! It preserves the source line numbering while removing the directive and
//! rejects misspelled or misplaced directives instead of silently ignoring them.

use serde_json::Value;

const OPTIONS_PREFIX: &str = "// @options:";
const MAX_TIMEOUT_MS: u64 = 2_147_483_647;
pub(super) const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedSource {
    pub(super) code: String,
    pub(super) max_output_tokens: u64,
    pub(super) timeout_ms: Option<u64>,
}

pub(super) fn parse_source(source: &str) -> Result<ParsedSource, String> {
    if source.trim().is_empty() {
        return Err("Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {\"max_output_tokens\": 1000}`.".to_string());
    }

    let misplaced = source
        .split('\n')
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.trim_start().starts_with(OPTIONS_PREFIX));
    if let Some((index, _)) = misplaced {
        return Err(format!(
            "The {OPTIONS_PREFIX} line must be the first line of the input; found one on line {}",
            index + 1
        ));
    }

    let (first_line, newline) = source.split_once('\n').unwrap_or((source, ""));
    let first_line = first_line.strip_suffix('\r').unwrap_or(first_line);
    let directive = first_line.trim_start();
    if !directive.starts_with(OPTIONS_PREFIX) {
        return Ok(ParsedSource {
            code: source.to_string(),
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            timeout_ms: None,
        });
    }

    let code = if source.contains('\n') {
        format!("\n{newline}")
    } else {
        String::new()
    };
    if code.trim().is_empty() {
        return Err(
            "The @options line must be followed by JavaScript source on subsequent lines"
                .to_string(),
        );
    }

    let value_text = directive[OPTIONS_PREFIX.len()..].trim();
    if value_text.is_empty() {
        return Err("@options must be a JSON object with supported fields `max_output_tokens` and `timeout_ms`".to_string());
    }
    let value: Value = serde_json::from_str(value_text).map_err(|error| format!(
        "@options must be valid JSON with supported fields `max_output_tokens` and `timeout_ms`: {error}"
    ))?;
    let fields = value.as_object().ok_or_else(|| "@options must be a JSON object with supported fields `max_output_tokens` and `timeout_ms`".to_string())?;
    for key in fields.keys() {
        if key != "max_output_tokens" && key != "timeout_ms" {
            return Err(format!(
                "@options only supports `max_output_tokens` and `timeout_ms`; got `{key}`"
            ));
        }
    }

    let max_output_tokens = match fields.get("max_output_tokens") {
        None => DEFAULT_MAX_OUTPUT_TOKENS,
        Some(Value::Number(number)) => number.as_u64().ok_or_else(|| {
            "@options field `max_output_tokens` must be a non-negative safe integer".to_string()
        })?,
        Some(_) => {
            return Err(
                "@options field `max_output_tokens` must be a non-negative safe integer"
                    .to_string(),
            );
        }
    };
    if max_output_tokens > 9_007_199_254_740_991 {
        return Err(
            "@options field `max_output_tokens` must be a non-negative safe integer".to_string(),
        );
    }
    let timeout_ms = match fields.get("timeout_ms") {
        None => None,
        Some(Value::Number(number)) => {
            let timeout = number.as_u64().filter(|timeout| *timeout > 0 && *timeout <= MAX_TIMEOUT_MS)
                .ok_or_else(|| format!("@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}"))?;
            Some(timeout)
        }
        Some(_) => {
            return Err(format!(
                "@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}"
            ));
        }
    };

    Ok(ParsedSource {
        code,
        max_output_tokens,
        timeout_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_line_options_and_keeps_line_number() {
        let parsed = parse_source(
            " // @options: {\"max_output_tokens\": 23, \"timeout_ms\": 100}\ntext('x');",
        )
        .unwrap();
        assert_eq!(parsed.code, "\ntext('x');");
        assert_eq!(parsed.max_output_tokens, 23);
        assert_eq!(parsed.timeout_ms, Some(100));
    }

    #[test]
    fn defaults_and_accepts_zero_token_budget() {
        let parsed = parse_source("text('x')").unwrap();
        assert_eq!(parsed.max_output_tokens, DEFAULT_MAX_OUTPUT_TOKENS);
        assert_eq!(parsed.timeout_ms, None);
        let parsed = parse_source("// @options: {\"max_output_tokens\": 0}\ntext('x')").unwrap();
        assert_eq!(parsed.max_output_tokens, 0);
    }

    #[test]
    fn rejects_misplaced_unknown_and_out_of_range_options() {
        for source in [
            "text('x')\n// @options: {}",
            "// @options: {\"other\": 1}\ntext('x')",
            "// @options: {\"max_output_tokens\": -1}\ntext('x')",
            "// @options: {\"max_output_tokens\": 9007199254740992}\ntext('x')",
            "// @options: {\"timeout_ms\": 2147483648}\ntext('x')",
            "// @options: {\"timeout_ms\": 0}\ntext('x')",
        ] {
            assert!(parse_source(source).is_err(), "accepted {source:?}");
        }
    }

    #[test]
    fn rejects_empty_script_and_malformed_directive() {
        for source in [
            "",
            "  ",
            "// @options: {}",
            "// @options: nope\ntext('x')",
            "{}\n// @options: {}",
        ] {
            assert!(parse_source(source).is_err(), "accepted {source:?}");
        }
    }
}
