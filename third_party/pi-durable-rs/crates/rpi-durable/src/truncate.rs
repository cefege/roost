//! Shared truncation utilities for tool outputs (port of
//! `packages/durable/src/truncate.ts` @ `abe508e1b`).
//!
//! Truncation is based on two independent limits — whichever is hit first
//! wins: a line limit (default 2000) and a byte limit (default 50KB). It never
//! returns partial lines; streaming tool output is bounded by
//! [`crate::harness::output`] instead.

/// Default line limit.
pub const DEFAULT_MAX_LINES: u64 = 2000;
/// Default byte limit.
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024;

/// Which limit a truncation hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    /// The line limit.
    Lines,
    /// The byte limit.
    Bytes,
}

/// Result of a head truncation (`TruncationResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    /// The truncated content.
    pub content: String,
    /// Whether truncation occurred.
    pub truncated: bool,
    /// Which limit was hit; `None` when not truncated.
    pub truncated_by: Option<TruncatedBy>,
    /// Total lines in the original content.
    pub total_lines: u64,
    /// Total bytes in the original content.
    pub total_bytes: u64,
    /// Complete lines in the truncated output.
    pub output_lines: u64,
    /// Bytes in the truncated output.
    pub output_bytes: u64,
    /// Whether the last line was partially truncated (tail edge case only).
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit.
    pub first_line_exceeds_limit: bool,
    /// The line limit that was applied.
    pub max_lines: u64,
    /// The byte limit that was applied.
    pub max_bytes: u64,
}

/// Limits of one truncation (`TruncationOptions`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TruncationOptions {
    /// Maximum lines; default [`DEFAULT_MAX_LINES`].
    pub max_lines: Option<u64>,
    /// Maximum bytes; default [`DEFAULT_MAX_BYTES`].
    pub max_bytes: Option<u64>,
}

/// UTF-8 byte length of `content`.
pub fn utf8_byte_length(content: &str) -> u64 {
    content.len() as u64
}

/// Format bytes as a human-readable size (upstream `formatSize`).
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Split for line counting: a trailing newline does not start an empty line.
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

/// Truncate content from the head (keep the first N lines/bytes).
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    truncate_head_of(
        content,
        split_lines_for_counting(content).len() as u64,
        utf8_byte_length(content),
        options,
    )
}

/// [`truncate_head`] of a text known by a prefix and its totals. The prefix
/// must be the whole text, or longer than `max_bytes + 1` UTF-8 bytes, or hold
/// at least `max_lines` newlines; then the result equals [`truncate_head`] of
/// the whole text.
pub fn truncate_head_of(
    prefix: &str,
    total_lines: u64,
    total_bytes: u64,
    options: TruncationOptions,
) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let lines = split_lines_for_counting(prefix);

    // Check if no truncation is needed.
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: prefix.to_string(),
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

    // Check if the first line alone exceeds the byte limit.
    let first_line_bytes = lines
        .first()
        .map(|line| utf8_byte_length(line))
        .unwrap_or(0);
    if first_line_bytes > max_bytes {
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

    // Collect complete lines that fit.
    let mut output: Vec<&str> = Vec::new();
    let mut output_bytes_count: u64 = 0;
    let mut truncated_by = TruncatedBy::Lines;
    for (index, line) in lines.iter().enumerate() {
        if index as u64 >= max_lines {
            break;
        }
        let line_bytes = utf8_byte_length(line) + if index > 0 { 1 } else { 0 }; // +1 for newline
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output.push(line);
        output_bytes_count += line_bytes;
    }

    // Without a byte break, only omitted lines prove the line limit was
    // reached; otherwise a trailing newline exceeded bytes.
    if truncated_by != TruncatedBy::Bytes && (output.len() as u64) >= total_lines {
        truncated_by = TruncatedBy::Bytes;
    }

    let output_content = output.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);
    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output.len() as u64,
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::output::character_end;

    #[test]
    fn counts_lines_like_upstream() {
        assert_eq!(split_lines_for_counting(""), Vec::<&str>::new());
        assert_eq!(split_lines_for_counting("a"), vec!["a"]);
        assert_eq!(split_lines_for_counting("a\n"), vec!["a"]);
        assert_eq!(split_lines_for_counting("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines_for_counting("a\n\n"), vec!["a", ""]);
    }

    #[test]
    fn leaves_content_within_limits_untouched() {
        let result = truncate_head("a\nb\n", TruncationOptions::default());
        assert!(!result.truncated);
        assert_eq!(result.content, "a\nb\n");
        assert_eq!(result.total_lines, 2);
        assert_eq!(result.total_bytes, 4);
        assert_eq!(result.output_lines, 2);
        assert_eq!(result.truncated_by, None);
    }

    #[test]
    fn truncates_by_lines() {
        let mut content = String::new();
        for index in 0..2500 {
            content.push_str(&format!("line {index}\n"));
        }
        let result = truncate_head(&content, TruncationOptions::default());
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.output_lines, 2000);
        assert_eq!(result.total_lines, 2500);
        assert!(result.content.ends_with("line 1999"));
        assert!(!result.first_line_exceeds_limit);
    }

    #[test]
    fn truncates_by_bytes_on_whole_lines() {
        let content = "aaa\nbbb\nccc\n";
        let result = truncate_head(
            content,
            TruncationOptions {
                max_lines: Some(10),
                max_bytes: Some(8),
            },
        );
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(result.content, "aaa\nbbb");
        assert_eq!(result.output_bytes, 7);
        assert_eq!(result.output_lines, 2);
    }

    #[test]
    fn reports_a_first_line_over_the_byte_limit() {
        let result = truncate_head(
            "a very long line",
            TruncationOptions {
                max_lines: Some(10),
                max_bytes: Some(4),
            },
        );
        assert!(result.first_line_exceeds_limit);
        assert_eq!(result.content, "");
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn formats_sizes_like_upstream() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1023), "1023B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(1024 * 1024), "1.0MB");
    }

    #[test]
    fn head_of_prefix_matches_head_of_whole_text() {
        let content = "a\nbb\nccc\ndddd\n";
        let prefix = &content[..content.len() - 2];
        let whole = truncate_head(
            content,
            TruncationOptions {
                max_lines: Some(3),
                max_bytes: Some(100),
            },
        );
        let known = truncate_head_of(
            prefix,
            split_lines_for_counting(content).len() as u64,
            utf8_byte_length(content),
            TruncationOptions {
                max_lines: Some(3),
                max_bytes: Some(100),
            },
        );
        assert_eq!(known.content, whole.content);
        assert_eq!(known.truncated_by, whole.truncated_by);
        let _ = character_end(&[], 0);
    }
}
