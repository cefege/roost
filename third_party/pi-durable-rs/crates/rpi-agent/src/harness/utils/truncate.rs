//! Port of `packages/agent/src/harness/utils/truncate.ts` @ pi 0.82.1 (2efa728) —
//! shared truncation utilities for tool outputs.
//!
//! Truncation is based on two independent limits — whichever is hit first wins
//! (truncate.ts:1-9):
//! - Line limit (default: 2000 lines)
//! - Byte limit (default: 50KB)
//!
//! Never returns partial lines (except the bash tail truncation edge case).
//!
//! Intentional differences:
//! - `utf8ByteLength` (truncate.ts:54-80) collapses to `str::len()`: a Rust
//!   `&str` is always valid UTF-8, so its byte length is exact and O(1) — the
//!   `Buffer` fast path, the no-Buffer fallback and the `nonAsciiPattern`
//!   (truncate.ts:47-52) have no Rust equivalent.
//! - Unpaired surrogates cannot exist in a Rust `&str`, so
//!   `replaceUnpairedSurrogates` (truncate.ts:89-110) and the 3-byte
//!   unpaired-surrogate accounting inside `utf8ByteLength` /
//!   `truncateStringToBytesFromEnd` are not ported. Their JS effect (count 3
//!   bytes, render U+FFFD) is unreachable on valid UTF-8.
//! - `truncateStringToBytesFromEnd` returns a `&str` borrow instead of a
//!   `String`; `replaceUnpairedSurrogates` never runs, so no allocation is
//!   needed.
//! - `truncateLine` keeps upstream's UTF-16 code-unit counting (`line.length`,
//!   `line.slice(0, maxChars)` — truncate.ts:346-349). Upstream `slice` may
//!   split a surrogate pair, yielding an unpaired surrogate; Rust cannot hold
//!   one, so the port stops before the pair instead (the pair is dropped, not
//!   replaced with U+FFFD).
//! - `formatSize` replicates JS `Number.prototype.toFixed(1)` rounding
//!   (round-half-up) with integer arithmetic; `format!("{:.1}")` rounds
//!   half-to-even and would diverge on exact `.x5` values (e.g. 1280 B →
//!   "1.3KB" vs "1.2KB").
//! - `TruncationOptions` uses `Option<usize>` fields mirroring the optional
//!   `maxLines` / `maxBytes` parameters; `Default` means "use defaults".

/// Default line limit (truncate.ts:11).
pub const DEFAULT_MAX_LINES: usize = 2000;
/// Default byte limit, 50KB (truncate.ts:12).
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// Max chars per grep match line (truncate.ts:13).
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Which limit was hit: `"lines" | "bytes"` (truncate.ts:21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

/// `TruncationResult` (truncate.ts:15-38), fields snake_cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    /// The truncated content.
    pub content: String,
    /// Whether truncation occurred.
    pub truncated: bool,
    /// Which limit was hit: `Some(Lines)`, `Some(Bytes)`, or `None` if not
    /// truncated.
    pub truncated_by: Option<TruncatedBy>,
    /// Total number of lines in the original content.
    pub total_lines: usize,
    /// Total number of bytes in the original content.
    pub total_bytes: usize,
    /// Number of complete lines in the truncated output.
    pub output_lines: usize,
    /// Number of bytes in the truncated output.
    pub output_bytes: usize,
    /// Whether the last line was partially truncated (only for tail truncation
    /// edge case).
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit (for head truncation).
    pub first_line_exceeds_limit: bool,
    /// The max lines limit that was applied.
    pub max_lines: usize,
    /// The max bytes limit that was applied.
    pub max_bytes: usize,
}

/// `TruncationOptions` (truncate.ts:40-45): optional `maxLines` / `maxBytes`,
/// defaulting to [`DEFAULT_MAX_LINES`] / [`DEFAULT_MAX_BYTES`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TruncationOptions {
    /// Maximum number of lines (default: 2000).
    pub max_lines: Option<usize>,
    /// Maximum number of bytes (default: 50KB).
    pub max_bytes: Option<usize>,
}

/// Return shape of [`truncate_line`] — `{ text, wasTruncated }`
/// (truncate.ts:344-345).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedLine {
    /// The (possibly truncated) text, with `... [truncated]` appended when
    /// truncation occurred.
    pub text: String,
    /// Whether truncation occurred.
    pub was_truncated: bool,
}

/// Port of `utf8ByteLength` (truncate.ts:54-80). A Rust `&str` is always valid
/// UTF-8, so `str::len()` is exactly `Buffer.byteLength(s, "utf8")`; the JS
/// no-Buffer fallback and unpaired-surrogate accounting (3 bytes each, like the
/// replacement character) cannot occur in Rust.
fn utf8_byte_length(content: &str) -> usize {
    content.len()
}

/// Port of `splitLinesForCounting` (truncate.ts:82-87): a trailing newline does
/// not count as an extra line, and the empty string has zero lines.
fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// `formatSize` (truncate.ts:115-123): format bytes as a human-readable size.
///
/// Mirrors JS `(bytes / 1024).toFixed(1)`, which rounds half away from zero for
/// positive values — computed with integer arithmetic (`(bytes * 10 +
/// divisor / 2) / divisor`) so exact `.x5` values round up like JS instead of
/// half-to-even like `format!("{:.1}")`.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        let tenths = (bytes as u128 * 10 + 512) / 1024;
        format!("{}.{}KB", tenths / 10, tenths % 10)
    } else {
        let tenths = (bytes as u128 * 10 + 512 * 1024) / (1024 * 1024);
        format!("{}.{}MB", tenths / 10, tenths % 10)
    }
}

/// `truncateHead` (truncate.ts:132-214): truncate content from the head (keep
/// the first N lines/bytes). Suitable for file reads where you want to see the
/// beginning.
///
/// Never returns partial lines. If the first line alone exceeds the byte
/// limit, returns empty content with `first_line_exceeds_limit = true`.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    // No truncation needed (truncate.ts:141-155).
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // First line alone exceeds the byte limit → nothing fits (truncate.ts:158-173).
    // `lines` is guaranteed non-empty here: the early return above would have
    // caught an empty string (0 lines, 0 bytes), so upstream's `lines[0]`
    // cannot throw.
    if let Some(first) = lines.first()
        && utf8_byte_length(first) > max_bytes
    {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    // Collect complete lines that fit (truncate.ts:175-191). The first line
    // carries no separator; every later one pays 1 byte for its leading "\n".
    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        let line_bytes = utf8_byte_length(line) + if i > 0 { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
    }

    // Line-limit exit keeps `truncated_by = "lines"` (truncate.ts:194-196). A
    // byte overflow can never flip it back: it leaves fewer than `max_lines`
    // entries.
    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// `truncateTail` (truncate.ts:222-295): truncate content from the tail (keep
/// the last N lines/bytes). Suitable for bash output where you want to see the
/// end (errors, final results).
///
/// May return a partial first line if the last line of the original content
/// exceeds the byte limit.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    // No truncation needed (truncate.ts:231-245).
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // Work backwards from the end (truncate.ts:247-272). Lines are collected in
    // reverse order (upstream `unshift`) and flipped back before `join` so the
    // output keeps the original order.
    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for i in (0..lines.len()).rev() {
        if output_lines_arr.len() >= max_lines {
            break;
        }
        let line = lines[i];
        let line_bytes = utf8_byte_length(line) + if output_lines_arr.is_empty() { 0 } else { 1 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Edge case (truncate.ts:259-267): nothing fits yet and this line
            // exceeds maxBytes → take the end of the line (partial).
            if output_lines_arr.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_lines_arr.push(truncated_line);
                output_bytes_count = utf8_byte_length(truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
    }

    // Same line-limit bookkeeping as head (truncate.ts:275-277). Mirrors the
    // upstream quirk where a byte-truncated tail that fills `max_lines` reports
    // `truncated_by = "lines"` (e.g. max_lines = 1 with a partial last line).
    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    output_lines_arr.reverse();
    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// `truncateStringToBytesFromEnd` (truncate.ts:301-336): the longest UTF-8
/// suffix of `s` fitting in `max_bytes`, scanning characters from the end so
/// multi-byte characters are never split.
///
/// Upstream additionally accounts for unpaired surrogates (3 bytes each,
/// replaced with U+FFFD afterwards); they cannot occur in a Rust `&str`, so the
/// char-boundary scan below is exact for every reachable input.
fn truncate_string_to_bytes_from_end(s: &str, max_bytes: usize) -> &str {
    if max_bytes == 0 {
        return "";
    }
    let bytes = s.as_bytes();
    let mut output_bytes = 0usize;
    let mut start = bytes.len();
    let mut i = bytes.len();
    while i > 0 {
        // Walk back over UTF-8 continuation bytes to the character start.
        let mut char_start = i - 1;
        while char_start > 0 && (bytes[char_start] & 0xc0) == 0x80 {
            char_start -= 1;
        }
        let char_bytes = i - char_start;
        if output_bytes + char_bytes > max_bytes {
            break;
        }
        output_bytes += char_bytes;
        start = char_start;
        i = char_start;
    }
    // `start` is always on a character boundary by construction, so slicing is
    // safe and cannot panic.
    &s[start..]
}

/// `truncateLine` (truncate.ts:342-350): truncate a single line to `max_chars`,
/// appending `... [truncated]` when it does not fit. Used for grep match lines;
/// the upstream default is [`GREP_MAX_LINE_LENGTH`].
///
/// `max_chars` counts UTF-16 code units, matching JS `line.length` /
/// `line.slice(0, maxChars)`. A surrogate pair is never split: upstream `slice`
/// can produce an unpaired surrogate here, which a Rust `&str` cannot hold (see
/// module header), so the port stops before the pair instead.
pub fn truncate_line(line: &str, max_chars: usize) -> TruncatedLine {
    if line.encode_utf16().count() <= max_chars {
        return TruncatedLine {
            text: line.to_string(),
            was_truncated: false,
        };
    }
    let mut units = 0usize;
    let mut end = 0usize;
    for (idx, ch) in line.char_indices() {
        let ch_units = ch.len_utf16();
        if units + ch_units > max_chars {
            break;
        }
        units += ch_units;
        end = idx + ch.len_utf8();
    }
    TruncatedLine {
        text: format!("{}... [truncated]", &line[..end]),
        was_truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All test intents below are ported from
    // `packages/agent/test/harness/truncate.test.ts` @ pi 0.82.1 (2efa728),
    // referenced per test. `test_format_size_*` / `test_truncate_line_*` and
    // the `test_truncates_*_on_line_limit*` cases are additions that pin
    // upstream behaviors the vitest suite does not exercise directly.
    //
    // The vitest `Buffer`-based helpers (byteLength / bufferTail /
    // assertMatchesBufferTail / sampledByteLimits, truncate.test.ts:6-62) are
    // ported below; the unpaired-surrogate fuzz alphabet entries are replaced
    // by valid 4-byte characters (unpaired surrogates cannot exist in a Rust
    // `&str`), keeping every 1/2/3/4-byte UTF-8 boundary value.

    /// `bufferTail` (truncate.test.ts:10-16): reference implementation — the
    /// longest byte tail of `content` (UTF-8) that fits `max_bytes`, cut at a
    /// UTF-8 character boundary, exactly like `Buffer.subarray().toString()`.
    fn buffer_tail(content: &str, max_bytes: usize) -> String {
        let bytes = content.as_bytes();
        if bytes.len() <= max_bytes {
            return content.to_string();
        }
        let mut start = bytes.len() - max_bytes;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
        }
        // `start` is always at a character boundary (see comment above), so the
        // slice is valid UTF-8.
        String::from_utf8(bytes[start..].to_vec()).unwrap()
    }

    /// `assertMatchesBufferTail` (truncate.test.ts:18-36).
    fn assert_matches_buffer_tail(input: &str, max_byte_values: &[usize]) {
        let total_bytes = input.len();
        let values: Vec<usize> = if max_byte_values.is_empty() {
            (0..=total_bytes + 5).collect()
        } else {
            max_byte_values.to_vec()
        };
        for max_bytes in values {
            let result = truncate_tail(
                input,
                TruncationOptions {
                    max_lines: Some(10),
                    max_bytes: Some(max_bytes),
                },
            );
            let expected = buffer_tail(input, max_bytes);
            assert_eq!(
                result.content, expected,
                "tail mismatch input={input:?} max_bytes={max_bytes}"
            );
            assert!(
                result.output_bytes <= max_bytes,
                "tail output exceeded byte limit input={input:?} max_bytes={max_bytes} output_bytes={}",
                result.output_bytes
            );
        }
    }

    /// `sampledByteLimits` (truncate.test.ts:38-62). The upstream `>= 0` filter
    /// is a no-op on `usize` and is dropped.
    fn sampled_byte_limits(input: &str) -> Vec<usize> {
        let total = input.len();
        let half = total / 2;
        let mut candidates = vec![
            0usize,
            1,
            2,
            3,
            4,
            5,
            8,
            half.saturating_sub(1),
            half,
            half + 1,
            total.saturating_sub(8),
            total.saturating_sub(5),
            total.saturating_sub(4),
            total.saturating_sub(3),
            total.saturating_sub(2),
            total.saturating_sub(1),
            total,
            total + 1,
            total + 4,
        ];
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }

    #[test]
    fn test_counts_utf8_bytes_without_node_buffer() {
        // "counts UTF-8 bytes without Node Buffer" (truncate.test.ts:65-73).
        let content = "aé🙂\nb";
        let result = truncate_head(
            content,
            TruncationOptions {
                max_bytes: Some(100),
                max_lines: Some(10),
            },
        );

        assert!(!result.truncated);
        assert_eq!(result.total_bytes, content.len());
        assert_eq!(result.output_bytes, content.len());
        assert_eq!(result.total_bytes, 9);
    }

    #[test]
    fn test_trailing_newline_not_counted_as_line() {
        // "does not count a trailing newline as an extra line"
        // (truncate.test.ts:75-82).
        let content = format!("{}\n", ["line"; 3].join("\n"));
        let head = truncate_head(
            &content,
            TruncationOptions {
                max_bytes: Some(100),
                max_lines: Some(3),
            },
        );
        let tail = truncate_tail(
            &content,
            TruncationOptions {
                max_bytes: Some(100),
                max_lines: Some(3),
            },
        );

        assert_eq!(
            (head.truncated, head.total_lines, head.output_lines),
            (false, 3, 3)
        );
        assert_eq!(
            (tail.truncated, tail.total_lines, tail.output_lines),
            (false, 3, 3)
        );
    }

    #[test]
    fn test_truncates_head_on_utf8_byte_limits_without_partial_lines() {
        // "truncates head on UTF-8 byte limits without partial lines"
        // (truncate.test.ts:84-93).
        let content = "éé\nabc";
        let result = truncate_head(
            content,
            TruncationOptions {
                max_bytes: Some(4),
                max_lines: Some(10),
            },
        );

        assert_eq!(result.content, "éé");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(result.output_bytes, 4);
        assert!(!result.first_line_exceeds_limit);
    }

    #[test]
    fn test_reports_head_truncation_when_first_line_exceeds_byte_limit() {
        // "reports head truncation when the first line exceeds the byte limit"
        // (truncate.test.ts:95-102).
        let result = truncate_head(
            "éé\nabc",
            TruncationOptions {
                max_bytes: Some(3),
                max_lines: Some(10),
            },
        );

        assert_eq!(result.content, "");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.first_line_exceeds_limit);
    }

    #[test]
    fn test_truncates_tail_on_utf8_boundaries_when_only_partial_last_line_fits() {
        // "truncates tail on UTF-8 boundaries when only a partial last line
        // fits" (truncate.test.ts:104-112).
        let result = truncate_tail(
            "aé🙂b",
            TruncationOptions {
                max_bytes: Some(5),
                max_lines: Some(10),
            },
        );

        assert_eq!(result.content, "🙂b");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.last_line_partial);
        assert_eq!(result.output_bytes, 5);
    }

    #[test]
    fn test_truncates_oversized_single_line_with_trailing_newline() {
        // "truncates an oversized single line with a trailing newline"
        // (truncate.test.ts:114-123).
        let input = format!("{}\n", "X".repeat(300_000));
        let result = truncate_tail(
            &input,
            TruncationOptions {
                max_bytes: Some(1024),
                max_lines: Some(100),
            },
        );

        assert_eq!(result.content, "X".repeat(1024));
        assert_eq!(result.output_bytes, 1024);
        assert_eq!(result.output_lines, 1);
        assert!(result.last_line_partial);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn test_drops_oversized_trailing_character_when_it_cannot_fit_in_tail_byte_limit() {
        // "drops an oversized trailing character when it cannot fit in tail
        // byte limit" (truncate.test.ts:125-133).
        let result = truncate_tail(
            "abc🙂",
            TruncationOptions {
                max_bytes: Some(3),
                max_lines: Some(10),
            },
        );

        assert_eq!(result.content, "");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.last_line_partial);
        assert_eq!(result.output_bytes, 0);
    }

    #[test]
    fn test_matches_buffer_tail_truncation_semantics_for_boundary_characters() {
        // "matches Buffer tail truncation semantics for surrogate edge cases"
        // (truncate.test.ts:135-138). Upstream feeds unpaired-surrogate strings
        // ("a\ud83d", "\ude42b", ...) which cannot exist in a Rust `&str`; the
        // intent — tail cuts never splitting a multi-byte character, including
        // 4-byte astral pairs — is exercised with valid equivalents covering
        // 2/3/4-byte characters at every boundary position.
        let inputs = [
            "a🙂",
            "🙂b",
            "a🙂b",
            "🙂🙂b",
            "🙂b🙂",
            "👩\u{200d}💻",
            "é中🙂",
            "🙂中é",
        ];
        for input in inputs {
            assert_matches_buffer_tail(input, &[]);
        }
    }

    #[test]
    fn test_matches_buffer_tail_truncation_semantics_across_deterministic_fuzz_cases() {
        // "matches Buffer tail truncation semantics across deterministic fuzz
        // cases" (truncate.test.ts:140-177). The four unpaired-surrogate
        // alphabet entries are replaced by valid 4-byte characters; the
        // 1/2/3/4-byte UTF-8 boundary values (0x7f, 0x80, 0x7ff, 0x800, 0xd7ff,
        // 0xe000, 0xffff, 0x10000, 0x10ffff) are all preserved.
        const ALPHABET: [&str; 13] = [
            "a",
            "\u{7f}",
            "\u{80}",
            "é",
            "\u{7ff}",
            "\u{800}",
            "中",
            "\u{d7ff}",
            "\u{e000}",
            "\u{ffff}",
            "\u{10000}",
            "\u{10ffff}",
            "🙂",
        ];

        fn check_exhaustive(prefix: &str, depth: usize) {
            assert_matches_buffer_tail(prefix, &sampled_byte_limits(prefix));
            if depth == 0 {
                return;
            }
            for character in ALPHABET {
                let extended = format!("{prefix}{character}");
                check_exhaustive(&extended, depth - 1);
            }
        }

        check_exhaustive("", 3);

        // Deterministic LCG with the same constants as the vitest fuzz
        // (truncate.test.ts:166-170): `seed = (seed * 1664525 + 1013904223) >>> 0`.
        let mut seed: u32 = 0x12345678;
        let mut random = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            f64::from(seed) / 4294967296.0
        };
        for _ in 0..1000 {
            let length = (random() * 80.0) as usize;
            let mut input = String::new();
            for _ in 0..length {
                let index = (random() * ALPHABET.len() as f64) as usize;
                input.push_str(ALPHABET[index]);
            }
            assert_matches_buffer_tail(&input, &sampled_byte_limits(&input));
        }
    }

    // -----------------------------------------------------------------------
    // Additions pinning upstream behaviors the vitest suite leaves untested.
    // -----------------------------------------------------------------------

    #[test]
    fn test_truncates_head_on_line_limit_keeping_first_lines() {
        let content = "l1\nl2\nl3\nl4\nl5";
        let result = truncate_head(
            content,
            TruncationOptions {
                max_bytes: Some(10_000),
                max_lines: Some(3),
            },
        );

        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.content, "l1\nl2\nl3");
        assert_eq!(result.total_lines, 5);
        assert_eq!(result.output_lines, 3);
        assert!(!result.last_line_partial);
        assert!(!result.first_line_exceeds_limit);
    }

    #[test]
    fn test_truncates_tail_on_line_limit_keeping_last_lines_in_order() {
        let content = "l1\nl2\nl3\nl4\nl5";
        let result = truncate_tail(
            content,
            TruncationOptions {
                max_bytes: Some(10_000),
                max_lines: Some(3),
            },
        );

        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.content, "l3\nl4\nl5");
        assert_eq!(result.total_lines, 5);
        assert_eq!(result.output_lines, 3);
    }

    #[test]
    fn test_truncate_tail_reports_lines_when_partial_line_fills_max_lines() {
        // Upstream quirk pinned by truncate.ts:275-277: when the byte-truncated
        // tail fills max_lines, `truncated_by` flips back to "lines" even
        // though the truncation was byte-driven.
        let result = truncate_tail(
            "abc",
            TruncationOptions {
                max_bytes: Some(2),
                max_lines: Some(1),
            },
        );

        assert_eq!(result.content, "bc");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.output_bytes, 2);
        assert!(result.last_line_partial);
    }

    #[test]
    fn test_format_size_matches_js_tofixed_rounding() {
        // JS `(bytes / 1024).toFixed(1)` rounds half up; Rust `{:.1}` rounds
        // half to even, so the exact `.x5` cases pin the integer-arithmetic
        // port (see `format_size`).
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(999), "999B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1279), "1.2KB");
        assert_eq!(format_size(1280), "1.3KB"); // 1.25 → "1.3", not "1.2"
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(1792), "1.8KB"); // 1.75 → "1.8"
        assert_eq!(format_size(1024 * 1024), "1.0MB");
        assert_eq!(format_size(1024 * 1024 + 262_144), "1.3MB"); // 1.25MB → "1.3"
        assert_eq!(format_size(2 * 1024 * 1024 + 262_144), "2.3MB");
    }

    #[test]
    fn test_truncate_line_keeps_short_lines_unchanged() {
        let result = truncate_line("short line", GREP_MAX_LINE_LENGTH);
        assert_eq!(result.text, "short line");
        assert!(!result.was_truncated);
    }

    #[test]
    fn test_truncate_line_truncates_long_lines_with_suffix() {
        let line = "x".repeat(GREP_MAX_LINE_LENGTH + 1);
        let result = truncate_line(&line, GREP_MAX_LINE_LENGTH);
        assert_eq!(
            result.text,
            format!("{}... [truncated]", "x".repeat(GREP_MAX_LINE_LENGTH))
        );
        assert!(result.was_truncated);
    }

    #[test]
    fn test_truncate_line_respects_custom_max_chars() {
        let result = truncate_line("abcdef", 3);
        assert_eq!(result.text, "abc... [truncated]");
        assert!(result.was_truncated);
    }

    #[test]
    fn test_truncate_line_counts_utf16_units_and_never_splits_surrogate_pairs() {
        // Upstream counts JS code units: an astral char costs 2. With
        // max_chars = 3, "🙂" (2 units) + "中" (1 unit) fit exactly.
        let result = truncate_line("🙂中abc", 3);
        assert_eq!(result.text, "🙂中... [truncated]");
        assert!(result.was_truncated);
        // The pair is never split: max_chars = 1 leaves no room for the 2-unit
        // emoji, so nothing before the suffix is kept (upstream would emit a
        // lone surrogate here — impossible in Rust, see module header).
        let result = truncate_line("🙂abc", 1);
        assert_eq!(result.text, "... [truncated]");
        assert!(result.was_truncated);
    }
}
