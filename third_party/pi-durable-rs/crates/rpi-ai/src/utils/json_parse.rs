//! Port of `packages/ai/src/utils/json-parse.ts` @ pi 0.82.1 (2efa728),
//! plus an embedded port of the `partial-json` 0.1.7 npm package
//! (`node_modules/partial-json/dist/index.js`) used by `parseStreamingJson`.
//!
//! Intentional differences:
//! - `Infinity` / `-Infinity` / `NaN` literals (accepted by the JS package for
//!   JS-ism reasons) are treated as malformed: `serde_json::Value` cannot
//!   represent them and they never occur in tool-call argument streams.
//! - The parser operates on `char`s; the JS version indexes UTF-16 code units.
//!   All decisions are per-character comparisons, so behavior is identical.
//! - Raw JSON text is pre-sanitized for unpaired `\uXXXX` surrogate escapes
//!   before `serde_json` parsing (JS `JSON.parse` accepts lone surrogates,
//!   `serde_json` rejects them). See [`strip_lone_surrogate_escapes`].

use serde_json::{Map, Value};

const VALID_JSON_ESCAPES: [char; 9] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

fn is_control_character(c: char) -> bool {
    (c as u32) <= 0x1f
}

fn escape_control_character(c: char) -> String {
    match c {
        '\u{08}' => "\\b".to_owned(),
        '\u{0c}' => "\\f".to_owned(),
        '\n' => "\\n".to_owned(),
        '\r' => "\\r".to_owned(),
        '\t' => "\\t".to_owned(),
        _ => format!("\\u{:04x}", c as u32),
    }
}

/// Byte-for-byte port of `repairJson`: escapes raw control characters inside
/// string literals and doubles backslashes before invalid escape characters.
///
/// The heavy caller (`parse_streaming_json_value`) runs this on every
/// streaming tool-args delta; the original always allocated a full output
/// copy even when the input was already clean (the common case while a
/// write/edit `content` string streams in). [`repair_json_if_needed`]
/// allocates only when a repair is actually required.
pub fn repair_json(json: &str) -> String {
    repair_json_if_needed(json).unwrap_or_else(|| json.to_owned())
}

/// Lazy [`repair_json`]: `None` when no repair is needed, `Some(repaired)`
/// otherwise. Output is byte-identical to [`repair_json`]. Until the first
/// repair the input is returned untouched (no allocation); from the first
/// repair onward every character flows into the materialized output.
fn repair_json_if_needed(json: &str) -> Option<String> {
    let chars: Vec<char> = json.chars().collect();
    let mut out: Option<String> = None;
    let mut in_string = false;
    let mut index = 0;

    /// Emits a repaired slice at `index`, materializing `out` with the
    /// untouched prefix `chars[..index]` on the first repair.
    fn repair(out: &mut Option<String>, chars: &[char], index: usize, replacement: &str) {
        if out.is_none() {
            let mut materialized = String::with_capacity(chars.len());
            materialized.extend(chars[..index].iter());
            *out = Some(materialized);
        }
        out.as_mut()
            .expect("materialized above")
            .push_str(replacement);
    }

    while index < chars.len() {
        let char = chars[index];

        if !in_string {
            if let Some(out) = out.as_mut() {
                out.push(char);
            }
            if char == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if char == '"' {
            in_string = false;
            if let Some(out) = out.as_mut() {
                out.push(char);
            }
            index += 1;
            continue;
        }

        if char == '\\' {
            let next_char = chars.get(index + 1).copied();
            match next_char {
                None => {
                    repair(&mut out, &chars, index, "\\\\");
                    index += 1;
                    continue;
                }
                Some('u') => {
                    let unicode_digits: String = chars.iter().skip(index + 2).take(4).collect();
                    if unicode_digits.len() == 4
                        && unicode_digits.chars().all(|c| c.is_ascii_hexdigit())
                    {
                        // Valid `\uXXXX` passes through untouched.
                        if let Some(out) = out.as_mut() {
                            out.push_str("\\u");
                            out.push_str(&unicode_digits);
                        }
                        index += 6;
                        continue;
                    }
                    // fall through to the generic escape check ('u' is valid)
                }
                _ => {}
            }

            let next_char = next_char.unwrap(); // invariant: None returned above
            if VALID_JSON_ESCAPES.contains(&next_char) {
                // Valid escape passes through untouched.
                if let Some(out) = out.as_mut() {
                    out.push('\\');
                    out.push(next_char);
                }
                index += 2;
                continue;
            }

            repair(&mut out, &chars, index, "\\\\");
            index += 1;
            continue;
        }

        if is_control_character(char) {
            let escaped = escape_control_character(char);
            repair(&mut out, &chars, index, &escaped);
        } else if let Some(out) = out.as_mut() {
            out.push(char);
        }
        index += 1;
    }

    out
}

/// Removes unpaired `\uXXXX` surrogate escapes (a high surrogate `\uD800`–
/// `\uDBFF` not immediately followed by a low-surrogate escape, or a low
/// surrogate `\uDC00`–`\uDFFF` not immediately preceded by one) from raw JSON
/// text, mirroring the net effect of JS `JSON.parse` (which accepts lone
/// surrogates) followed by `sanitizeSurrogates` (which drops them).
///
/// Only escape sequences are inspected; raw UTF-8 text cannot contain
/// surrogates. Paired escapes (valid astral characters) are preserved.
pub fn strip_lone_surrogate_escapes(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut out = String::new();
    let mut in_string = false;
    let mut index = 0;

    // Reads the 4 hex digits after a `\u` at `slash` (index of the backslash).
    let hex4 = |slash: usize| -> Option<u16> {
        let digits: String = chars.iter().skip(slash + 2).take(4).collect();
        if digits.len() == 4 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
            u16::from_str_radix(&digits, 16).ok()
        } else {
            None
        }
    };

    while index < chars.len() {
        let c = chars[index];
        if !in_string {
            out.push(c);
            if c == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }
        if c == '\\' {
            if chars.get(index + 1) == Some(&'u')
                && let Some(code) = hex4(index)
            {
                if (0xd800..0xdc00).contains(&code) {
                    // High surrogate: keep only when immediately followed
                    // by a low-surrogate escape.
                    let next_slash = index + 6;
                    let paired = chars.get(next_slash) == Some(&'\\')
                        && chars.get(next_slash + 1) == Some(&'u')
                        && hex4(next_slash)
                            .map(|low| (0xdc00..0xe000).contains(&low))
                            .unwrap_or(false);
                    if paired {
                        out.push_str(&chars[index..index + 6].iter().collect::<String>());
                    }
                    index += 6;
                    continue;
                }
                if (0xdc00..0xe000).contains(&code) {
                    // Low surrogate: keep only when immediately preceded
                    // by a high-surrogate escape. The preceding pair was
                    // already emitted, so check the tail of `out`.
                    let mut tail = out.chars().rev().take(6).collect::<Vec<char>>();
                    tail.reverse();
                    let preceded = tail.len() == 6
                        && tail[0] == '\\'
                        && tail[1] == 'u'
                        && u16::from_str_radix(&tail[2..6].iter().collect::<String>(), 16)
                            .map(|high| (0xd800..0xdc00).contains(&high))
                            .unwrap_or(false);
                    if preceded {
                        out.push_str(&chars[index..index + 6].iter().collect::<String>());
                    }
                    index += 6;
                    continue;
                }
            }
            // Non-`\u` escape: emit both characters so an escaped quote does
            // not end the string literal (e.g. `\"` or `\\`).
            out.push(c);
            if let Some(&next) = chars.get(index + 1) {
                out.push(next);
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        out.push(c);
        if c == '"' {
            in_string = false;
        }
        index += 1;
    }

    out
}

/// Parses JSON text, tolerating lone-surrogate escapes like JS `JSON.parse`.
fn json_parse_lenient(text: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(first) => {
            let stripped = strip_lone_surrogate_escapes(text);
            if stripped != text {
                serde_json::from_str(&stripped)
            } else {
                Err(first)
            }
        }
    }
}

/// Port of `parseJsonWithRepair`: strict parse, then one repair retry.
pub fn parse_json_with_repair(json: &str) -> Result<Value, serde_json::Error> {
    match json_parse_lenient(json) {
        Ok(value) => Ok(value),
        Err(first) => match repair_json_if_needed(json) {
            // A materialized repair differs from the input by construction;
            // its parse result stands. No repair needed (streaming-truncated
            // but otherwise clean JSON) keeps the first error without an
            // alloc-and-compare round trip.
            Some(repaired) => json_parse_lenient(&repaired),
            None => Err(first),
        },
    }
}

// ---------------------------------------------------------------------------
// partial-json 0.1.7 port (Allow.ALL)
// ---------------------------------------------------------------------------

/// Minimal recursive-descent partial JSON parser mirroring `partial-json`
/// with `Allow.ALL`: every construct may be truncated. Returns `None` where
/// the JS package throws (`PartialJSON` cannot occur with Allow.ALL at the
/// top level except through malformed input).
struct PartialParser {
    chars: Vec<char>,
    index: usize,
}

impl PartialParser {
    fn len(&self) -> usize {
        self.chars.len()
    }

    fn at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    /// JS `String.prototype.substring(from, to)`: clamps then swaps.
    fn substring(&self, from: usize, to: usize) -> String {
        let len = self.len();
        let a = from.min(len);
        let b = to.min(len);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.chars[lo..hi].iter().collect()
    }

    fn remaining(&self) -> String {
        self.chars[self.index..].iter().collect()
    }

    fn last_index_of(&self, needle: char) -> Option<usize> {
        self.chars.iter().rposition(|&c| c == needle)
    }

    fn skip_blank(&mut self) {
        while let Some(c) = self.at(self.index) {
            if matches!(c, ' ' | '\n' | '\r' | '\t') {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    fn parse_any(&mut self) -> Option<Value> {
        self.skip_blank();
        if self.index >= self.len() {
            return None; // PartialJSON "Unexpected end of input"
        }
        let c = self.at(self.index)?;
        match c {
            '"' => self.parse_str(),
            '{' => self.parse_obj(),
            '[' => self.parse_arr(),
            _ => {
                let remaining = self.remaining();
                for (literal, value) in [
                    ("null", Value::Null),
                    ("true", Value::Bool(true)),
                    ("false", Value::Bool(false)),
                ] {
                    if remaining.starts_with(literal) {
                        self.index += literal.len();
                        return Some(value);
                    }
                    // Allow partial literal prefixes (Allow.NULL / Allow.BOOL).
                    if remaining.len() < literal.len() && literal.starts_with(&remaining) {
                        self.index += literal.len();
                        return Some(value);
                    }
                }
                self.parse_num()
            }
        }
    }

    fn parse_str(&mut self) -> Option<Value> {
        let start = self.index;
        let mut escape = false;
        self.index += 1; // skip initial quote
        while self.index < self.len()
            && (self.at(self.index) != Some('"')
                || (escape && self.at(self.index.wrapping_sub(1)) == Some('\\')))
        {
            escape = self.at(self.index) == Some('\\') && !escape;
            self.index += 1;
        }
        if self.at(self.index) == Some('"') {
            self.index += 1;
            let end = self.index - usize::from(escape);
            return json_parse_lenient(&self.substring(start, end)).ok();
        }
        // Allow.STR: unterminated strings are completed with a closing quote.
        let end = self.index - usize::from(escape);
        let candidate = format!("{}\"", self.substring(start, end));
        if let Ok(value) = json_parse_lenient(&candidate) {
            return Some(value);
        }
        // Invalid escape sequence fallback: cut at the last backslash.
        let last_backslash = self.last_index_of('\\')?;
        let candidate = format!("{}\"", self.substring(start, last_backslash));
        json_parse_lenient(&candidate).ok()
    }

    fn parse_obj(&mut self) -> Option<Value> {
        self.index += 1; // skip initial brace
        self.skip_blank();
        let mut obj = Map::new();
        loop {
            if self.at(self.index) == Some('}') {
                break;
            }
            self.skip_blank();
            if self.index >= self.len() {
                return Some(Value::Object(obj)); // Allow.OBJ
            }
            // Key-parse errors are caught by the object-level Allow.OBJ.
            let key = match self.parse_str() {
                Some(Value::String(key)) => key,
                _ => return Some(Value::Object(obj)),
            };
            self.skip_blank();
            self.index += 1; // skip colon (unchecked upstream)
            match self.parse_any() {
                Some(value) => {
                    obj.insert(key, value);
                }
                None => return Some(Value::Object(obj)), // Allow.OBJ
            }
            self.skip_blank();
            if self.at(self.index) == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1; // skip final brace
        Some(Value::Object(obj))
    }

    fn parse_arr(&mut self) -> Option<Value> {
        self.index += 1; // skip initial bracket
        let mut arr = Vec::new();
        loop {
            if self.at(self.index) == Some(']') {
                break;
            }
            match self.parse_any() {
                Some(value) => arr.push(value),
                None => return Some(Value::Array(arr)), // Allow.ARR
            }
            self.skip_blank();
            if self.at(self.index) == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1; // skip final bracket
        Some(Value::Array(arr))
    }

    fn parse_num(&mut self) -> Option<Value> {
        if self.index == 0 {
            // Whole-input number (parse() called on a bare numeric string).
            if self.remaining() == "-" {
                return None; // MalformedJSON "Not sure what '-' is"
            }
            if let Ok(value) = json_parse_lenient(&self.remaining()) {
                return Some(value);
            }
            // Allow.NUM: cut at the last 'e' (partial exponent).
            let last_e = self.last_index_of('e')?;
            return json_parse_lenient(&self.substring(0, last_e)).ok();
        }

        let start = self.index;
        if self.at(self.index) == Some('-') {
            self.index += 1;
        }
        while let Some(c) = self.at(self.index) {
            if ",]}".contains(c) {
                break;
            }
            self.index += 1;
        }
        // index == len is fine under Allow.NUM.
        let slice = self.substring(start, self.index);
        if let Ok(value) = json_parse_lenient(&slice) {
            return Some(value);
        }
        if slice == "-" {
            return None; // PartialJSON "Not sure what '-' is"
        }
        let last_e = self.last_index_of('e')?;
        json_parse_lenient(&self.substring(start, last_e)).ok()
    }
}

/// Port of `partial-json`'s `parse` with `Allow.ALL`.
fn partial_parse(partial_json: &str) -> Option<Value> {
    let trimmed = partial_json.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parser = PartialParser {
        chars: trimmed.chars().collect(),
        index: 0,
    };
    parser.parse_any()
}

/// Port of `parseStreamingJson`: parses potentially incomplete JSON during
/// streaming. Always returns an object (`{}` when nothing parses), like the
/// upstream default generic `Record<string, unknown>`.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Map<String, Value> {
    match parse_streaming_json_value(partial_json) {
        Value::Object(map) => map,
        // Upstream assigns whatever partial-json returns (even a bare string);
        // `ToolCall.arguments` is an object in rpi, so non-object partials
        // degrade to `{}` (transient partial display only).
        _ => Map::new(),
    }
}

/// [`parse_streaming_json`] variant preserving non-object results.
pub fn parse_streaming_json_value(partial_json: Option<&str>) -> Value {
    let Some(text) = partial_json else {
        return Value::Object(Map::new());
    };
    if text.trim().is_empty() {
        return Value::Object(Map::new());
    }
    if let Ok(value) = parse_json_with_repair(text) {
        return value;
    }
    if let Some(value) = partial_parse(text) {
        return value;
    }
    if let Some(value) = partial_parse(&repair_json(text)) {
        return value;
    }
    Value::Object(Map::new())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn parse(text: &str) -> Value {
        parse_streaming_json_value(Some(text))
    }

    /// The lazy repair must be byte-identical to the eager original on both
    /// clean inputs (no allocation, `None`) and dirty inputs.
    #[test]
    fn test_lazy_repair_matches_eager_output() {
        let clean = r#"{"path":"src/main.rs","content":"fn main() {}\n"}"#;
        assert_eq!(
            repair_json_if_needed(clean),
            None,
            "clean input needs no repair"
        );
        assert_eq!(repair_json(clean), clean.to_owned());

        let dirty_inputs = [
            // Raw control characters inside strings.
            "{\"a\":\"line1\nline2\t\"}",
            "{\"a\":\"x\u{0c}y\"}",
            // Control char before any repair (prefix must be preserved).
            "{\"key\":\"value\",\"other\":\"z\rz\"}",
            // Invalid escape doubled; valid escapes untouched.
            "{\"a\":\"c:\\x\"}",
            "{\"a\":\"c:\\n\\t\"}",
            // Trailing lone backslash.
            "{\"a\":\"c:\\\"",
            // Valid \uXXXX kept; invalid hex after \u doubled.
            "{\"a\":\"\\u00e9\"}",
            "{\"a\":\"\\uZZZZ\"}",
            // Escapes outside strings are untouched.
            "{\"a\":\"b\"}",
        ];
        for dirty in dirty_inputs {
            let eager = eager_repair_reference(dirty);
            match repair_json_if_needed(dirty) {
                None => assert_eq!(
                    eager,
                    dirty.to_owned(),
                    "no lazy repair ⇒ eager output must equal the input for {dirty:?}"
                ),
                Some(lazy) => assert_eq!(
                    lazy, eager,
                    "lazy repair must match the eager original for {dirty:?}"
                ),
            }
            assert_eq!(repair_json(dirty), eager);
        }
    }

    /// The pre-lazy eager implementation, kept as the behavioral reference
    /// (always builds the full output).
    fn eager_repair_reference(json: &str) -> String {
        let chars: Vec<char> = json.chars().collect();
        let mut repaired = String::new();
        let mut in_string = false;
        let mut index = 0;
        while index < chars.len() {
            let char = chars[index];
            if !in_string {
                repaired.push(char);
                if char == '"' {
                    in_string = true;
                }
                index += 1;
                continue;
            }
            if char == '"' {
                repaired.push(char);
                in_string = false;
                index += 1;
                continue;
            }
            if char == '\\' {
                let next_char = chars.get(index + 1).copied();
                match next_char {
                    None => {
                        repaired.push_str("\\\\");
                        index += 1;
                        continue;
                    }
                    Some('u') => {
                        let unicode_digits: String = chars.iter().skip(index + 2).take(4).collect();
                        if unicode_digits.len() == 4
                            && unicode_digits.chars().all(|c| c.is_ascii_hexdigit())
                        {
                            repaired.push_str("\\u");
                            repaired.push_str(&unicode_digits);
                            index += 6;
                            continue;
                        }
                    }
                    _ => {}
                }
                let next_char = next_char.unwrap();
                if VALID_JSON_ESCAPES.contains(&next_char) {
                    repaired.push('\\');
                    repaired.push(next_char);
                    index += 2;
                    continue;
                }
                repaired.push_str("\\\\");
                index += 1;
                continue;
            }
            if is_control_character(char) {
                repaired.push_str(&escape_control_character(char));
            } else {
                repaired.push(char);
            }
            index += 1;
        }
        repaired
    }

    #[test]
    fn test_repair_json_escapes_control_characters() {
        let input = "{\"a\":\"line1\nline2\t\"}";
        let repaired = repair_json(input);
        assert_eq!(repaired, "{\"a\":\"line1\\nline2\\t\"}");
        let value = parse_json_with_repair(input).expect("repaired parses");
        assert_eq!(value, json!({"a": "line1\nline2\t"}));
    }

    #[test]
    fn test_repair_json_doubles_invalid_backslash() {
        // `\x` is not a valid JSON escape: the backslash is doubled.
        let input = "{\"a\":\"c:\\x\"}";
        assert!(serde_json::from_str::<Value>(input).is_err());
        let value = parse_json_with_repair(input).expect("repaired parses");
        assert_eq!(value, json!({"a": "c:\\x"}));
    }

    #[test]
    fn test_repair_json_preserves_valid_escapes() {
        let input = "{\"a\":\"\\n\\u00e9\"}";
        assert_eq!(repair_json(input), input);
    }

    #[test]
    fn test_parse_streaming_json_empty_inputs() {
        assert_eq!(parse_streaming_json(None), Map::new());
        assert_eq!(parse_streaming_json(Some("")), Map::new());
        assert_eq!(parse_streaming_json(Some("   ")), Map::new());
    }

    #[test]
    fn test_parse_streaming_json_complete() {
        assert_eq!(parse("{\"a\":1}"), json!({"a": 1}));
    }

    #[test]
    fn test_parse_streaming_json_partial_any_split() {
        // Arbitrary splits of a tool-call payload must parse to something
        // object-shaped; the full string must match the strict parse.
        let full = "{\"command\":\"ls -la\",\"timeout\":30,\"nested\":{\"x\":[1,2,3]}}";
        let value: Value = serde_json::from_str(full).expect("valid");
        for split in 0..=full.len() {
            if !full.is_char_boundary(split) {
                continue;
            }
            let partial = parse(&full[..split]);
            if split == full.len() {
                assert_eq!(partial, value);
            }
        }
        assert_eq!(parse("{\"command\":\"ls -l"), json!({"command": "ls -l"}));
        assert_eq!(parse("{\"command\":"), json!({}));
        assert_eq!(
            parse("{\"command\":\"ls -la\",\"timeout\":3"),
            json!({"command": "ls -la", "timeout": 3})
        );
        assert_eq!(
            parse("{\"nested\":{\"x\":[1,2"),
            json!({"nested": {"x": [1, 2]}})
        );
    }

    #[test]
    fn test_parse_streaming_json_partial_literals() {
        assert_eq!(parse("{\"a\":tru"), json!({"a": true}));
        assert_eq!(parse("{\"a\":nul"), json!({"a": null}));
        assert_eq!(parse("{\"a\":fals"), json!({"a": false}));
        assert_eq!(parse("{\"a\":1e"), json!({"a": 1}));
    }

    #[test]
    fn test_parse_streaming_json_trailing_backslash() {
        assert_eq!(parse("{\"a\":\"abc\\"), json!({"a": "abc"}));
        assert_eq!(parse("{\"a\":\"abc"), json!({"a": "abc"}));
    }

    #[test]
    fn test_parse_streaming_json_malformed_returns_empty() {
        assert_eq!(parse("not json at all"), json!({}));
        assert_eq!(parse("{\"a\":@}"), json!({}));
        assert_eq!(parse_streaming_json(Some("not json at all")), Map::new());
    }

    #[test]
    fn test_strip_lone_surrogate_escapes() {
        // Paired surrogate escapes are preserved.
        let paired = "{\"a\":\"\\ud83d\\ude00\"}";
        assert_eq!(strip_lone_surrogate_escapes(paired), paired);
        // Lone high / low surrogates are dropped.
        assert_eq!(
            strip_lone_surrogate_escapes("{\"a\":\"x\\ud83dy\"}"),
            "{\"a\":\"xy\"}"
        );
        assert_eq!(
            strip_lone_surrogate_escapes("{\"a\":\"x\\ude00y\"}"),
            "{\"a\":\"xy\"}"
        );
        // High surrogate followed by a non-escape character: dropped.
        assert_eq!(
            strip_lone_surrogate_escapes("{\"a\":\"\\ud83dx\"}"),
            "{\"a\":\"x\"}"
        );
        let value = parse_json_with_repair("{\"a\":\"x\\ud83dy\"}").expect("lenient parse");
        assert_eq!(value, json!({"a": "xy"}));
    }
}
