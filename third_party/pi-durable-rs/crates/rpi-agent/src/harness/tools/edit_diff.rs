//! Port of `packages/agent/src/harness/tools/edit-diff.ts` @ pi 0.82.1
//! (2efa728) — shared diff computation, fuzzy matching, and text-replacement
//! utilities for the edit tool.
//!
//! # Index space note
//!
//! JavaScript `String.indexOf` / `String.length` operate on UTF-16 code units.
//! Rust string operations operate on bytes (`usize`). Throughout this module we
//! use byte indices for matching and slicing. Because matching and slicing stay
//! within the same index space (byte offsets into a `&str`), the resulting text
//! output is identical to the upstream JavaScript implementation.
//!
//! Intentional differences:
//! - Upstream `throw new Error(message)` becomes `Err(AgentError::Message)`,
//!   keeping the message text verbatim.
//! - `generateUnifiedPatch` delegates to jsdiff's `createTwoFilesPatch`
//!   upstream (edit-diff.ts:366-371); here the unified patch is produced by a
//!   hand-rolled Myers line diff (the `diff` crate is not part of the
//!   workspace baseline). The output format matches jsdiff's
//!   `FILE_HEADERS_ONLY` mode: `--- file` / `+++ file` headers, `@@ -a,b +c,d
//!   @@` hunks, and `\ No newline at end of file` markers.
//! - The shared diff machinery (`splitLinesWithEndings` /
//!   `getLineSpans` / `getReplacementLineRange` / `applyReplacements` /
//!   `applyReplacementsPreservingUnchangedLines` / `fuzzyFindText` /
//!   `countOccurrences` / `applyEditsToNormalizedContent` /
//!   `generateDiffString`) follows edit-diff.ts:53-500 line by line; the
//!   line-diff engine behind `generateDiffString` is a Myers algorithm with
//!   jsdiff's removed-before-added part ordering and context folding. The
//!   edit script is byte-identical to jsdiff 8.x (verified against the
//!   golden corpus and differential tests), while the space bound is
//!   O(D log D) instead of the classic O(D²) trace — see `myers_diff`.

use unicode_normalization::UnicodeNormalization;

use crate::error::AgentError;

// ---------------------------------------------------------------------------
// Line-ending helpers (edit-diff.ts:7-21)
// ---------------------------------------------------------------------------

/// `detectLineEnding` (edit-diff.ts:7-13): the first `\r\n` before the first
/// lone `\n` wins; default `"\n"`.
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

/// `normalizeToLF` (edit-diff.ts:15-17).
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `restoreLineEndings` (edit-diff.ts:19-21).
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

// ---------------------------------------------------------------------------
// BOM (edit-diff.ts:243-246)
// ---------------------------------------------------------------------------

/// `stripBom` (edit-diff.ts:243-246) — return `(bom, text)`.
pub fn strip_bom(content: &str) -> (String, String) {
    if let Some(rest) = content.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}".to_string(), rest.to_string())
    } else {
        (String::new(), content.to_string())
    }
}

// ---------------------------------------------------------------------------
// Fuzzy normalization (edit-diff.ts:23-51)
// ---------------------------------------------------------------------------

/// `normalizeForFuzzyMatch` (edit-diff.ts:30-51): progressive transformations —
/// NFKC, per-line trailing whitespace stripping, smart quotes → ASCII, Unicode
/// dashes → `-`, special Unicode spaces → regular space.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let trimmed: String = nfkc
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    trimmed
        .chars()
        .map(|c| match c {
            // Smart single quotes → '
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            // Smart double quotes → "
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            // Various dashes/hyphens → -
            // U+2010 hyphen, U+2011 non-breaking hyphen, U+2012 figure dash,
            // U+2013 en-dash, U+2014 em-dash, U+2015 horizontal bar, U+2212 minus
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            // Special spaces → regular space
            // U+00A0 NBSP, U+2002-U+200A various spaces, U+202F narrow NBSP,
            // U+205F medium math space, U+3000 ideographic space
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            _ => c,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Line splitting and spans (edit-diff.ts:53-81)
// ---------------------------------------------------------------------------

/// `splitLinesWithEndings` (edit-diff.ts:53-55): the JS regex
/// `/[^\n]*\n|[^\n]+/g` — each line keeps its trailing `\n` when present; the
/// empty string yields no lines.
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            result.push(&content[start..=i]);
            start = i + 1;
        }
    }
    if start < content.len() {
        result.push(&content[start..]);
    }
    result
}

/// `LineSpan` (edit-diff.ts:57-60).
#[derive(Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// `getLineSpans` (edit-diff.ts:71-78).
fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Replacement types (edit-diff.ts:62-69)
// ---------------------------------------------------------------------------

/// `Edit` (edit-diff.ts:187-190) — one targeted replacement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

/// `AppliedEditsResult` (edit-diff.ts:192-195).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    /// The original (LF-normalized) content before edits.
    pub base_content: String,
    /// The content after the edits have been applied.
    pub new_content: String,
}

/// `MatchedEdit` (edit-diff.ts:62-67).
#[derive(Clone)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

/// `TextReplacement` (edit-diff.ts:69).
type TextReplacement = MatchedEdit;

// ---------------------------------------------------------------------------
// Line-range computation (edit-diff.ts:80-105)
// ---------------------------------------------------------------------------

/// `getReplacementLineRange` (edit-diff.ts:80-105). Upstream throws when the
/// range is outside the base content; the range of a match found *inside* the
/// content can never be outside, so the error is a defensive invariant.
fn get_replacement_line_range(
    lines: &[LineSpan],
    replacement: &TextReplacement,
) -> Result<(usize, usize), AgentError> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let mut start_line = None;
    for (i, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(i);
            break;
        }
    }
    let Some(start_line) = start_line else {
        return Err(AgentError::Message(
            "Replacement range is outside the base content.".to_string(),
        ));
    };

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(AgentError::Message(
            "Replacement range is outside the base content.".to_string(),
        ));
    }

    Ok((start_line, end_line + 1))
}

// ---------------------------------------------------------------------------
// Applying replacements (edit-diff.ts:107-169)
// ---------------------------------------------------------------------------

/// `applyReplacements` (edit-diff.ts:107-116): apply in reverse order so
/// earlier offsets stay valid. `offset` is subtracted from each `match_index`
/// because the content may be a slice of a larger string.
fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index.saturating_sub(offset);
        let before = &result[..match_index];
        let after = &result[match_index + replacement.match_length..];
        result = format!("{before}{}{after}", replacement.new_text);
    }
    result
}

/// `applyReplacementsPreservingUnchangedLines` (edit-diff.ts:128-169): apply
/// replacements matched against `base_content` to `original_content`, keeping
/// unchanged line blocks from the original (so duplicate normalized lines
/// cannot be aligned to the wrong occurrence).
fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String, AgentError> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(AgentError::Message(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .to_string(),
        ));
    }

    // Build groups of adjacent/overlapping replacements (edit-diff.ts:139-150).
    let mut sorted_replacements: Vec<&TextReplacement> = replacements.iter().collect();
    sorted_replacements.sort_by_key(|r| r.match_index);
    let mut groups: Vec<(usize, usize, Vec<TextReplacement>)> = Vec::new(); // (startLine, endLine, replacements)
    for replacement in sorted_replacements {
        let range = get_replacement_line_range(&base_lines, replacement)?;
        if let Some((_, end_line, group_replacements)) = groups.last_mut()
            && range.0 < *end_line
        {
            *end_line = (*end_line).max(range.1);
            group_replacements.push(replacement.clone());
            continue;
        }
        groups.push((range.0, range.1, vec![replacement.clone()]));
    }

    let mut original_line_index = 0;
    let mut result = String::new();
    for (start_line, end_line, group_replacements) in &groups {
        // Copy original lines before this group (edit-diff.ts:155).
        for &line in &original_lines[original_line_index..*start_line] {
            result.push_str(line);
        }

        // Apply replacements within the group slice of base_content
        // (edit-diff.ts:157-163).
        let group_start_offset = base_lines[*start_line].start;
        let group_end_offset = base_lines[*end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            group_replacements,
            group_start_offset,
        ));

        original_line_index = *end_line;
    }
    // Copy remaining original lines (edit-diff.ts:166).
    for &line in &original_lines[original_line_index..] {
        result.push_str(line);
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Fuzzy matching (edit-diff.ts:171-241)
// ---------------------------------------------------------------------------

/// `FuzzyMatchResult` (edit-diff.ts:171-185) — `contentForReplacement` is not
/// needed in Rust: the caller passes the normalized content explicitly.
struct FuzzyMatchResult {
    found: bool,
    /// Byte index where the match starts.
    index: usize,
    /// Byte length of the matched text.
    match_length: usize,
    /// Whether fuzzy matching was used (false = exact match).
    used_fuzzy_match: bool,
}

/// `fuzzyFindText` (edit-diff.ts:203-241): exact match first, then matching
/// entirely in NFKC-normalized space.
fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if let Some(fuzzy_index) = fuzzy_content.find(&fuzzy_old_text) {
        return FuzzyMatchResult {
            found: true,
            index: fuzzy_index,
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
        };
    }

    FuzzyMatchResult {
        found: false,
        index: 0,
        match_length: 0,
        used_fuzzy_match: false,
    }
}

/// `countOccurrences` (edit-diff.ts:248-252): always counted in fuzzy-
/// normalized space (edit-diff.ts:248-252) so fuzzy-normalized duplicates are
/// detected even after an exact match. Non-overlapping, like JS `split().length
/// - 1`.
fn count_occurrences(content: &str, old_text: &str) -> usize {
    if old_text.is_empty() {
        return 0;
    }
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    fuzzy_content.matches(&fuzzy_old_text).count()
}

// ---------------------------------------------------------------------------
// Error message builders (edit-diff.ts:254-290)
// ---------------------------------------------------------------------------

fn get_not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

fn get_duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> String {
    if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn get_empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn get_no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

// ---------------------------------------------------------------------------
// apply_edits_to_normalized_content (edit-diff.ts:301-363)
// ---------------------------------------------------------------------------

/// `applyEditsToNormalizedContent` (edit-diff.ts:301-363): all edits are
/// matched against the same original content; replacements are applied in
/// reverse order so offsets stay stable. If any edit needs fuzzy matching, the
/// whole operation runs in fuzzy-normalized space and the changes are then
/// overlaid onto the original content line by line.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, AgentError> {
    let normalized_edits: Vec<(String, String)> = edits
        .iter()
        .map(|edit| {
            (
                normalize_to_lf(&edit.old_text),
                normalize_to_lf(&edit.new_text),
            )
        })
        .collect();

    for (i, (old_text, _)) in normalized_edits.iter().enumerate() {
        if old_text.is_empty() {
            return Err(AgentError::Message(get_empty_old_text_error(
                path,
                i,
                normalized_edits.len(),
            )));
        }
    }

    // Initial matching pass: decide whether any edit needs fuzzy matching
    // (edit-diff.ts:317-319).
    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|(old_text, _)| fuzzy_find_text(normalized_content, old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|m| m.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (i, (old_text, new_text)) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, old_text);
        if !match_result.found {
            return Err(AgentError::Message(get_not_found_error(
                path,
                i,
                normalized_edits.len(),
            )));
        }

        let occurrences = count_occurrences(&replacement_base_content, old_text);
        if occurrences > 1 {
            return Err(AgentError::Message(get_duplicate_error(
                path,
                i,
                normalized_edits.len(),
                occurrences,
            )));
        }

        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: new_text.clone(),
        });
    }

    // Overlap rejection on the sort-by-position order (edit-diff.ts:342-351).
    matched_edits.sort_by_key(|m| m.match_index);
    for pair in matched_edits.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(AgentError::Message(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            )));
        }
    }

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(
            normalized_content,
            &replacement_base_content,
            &matched_edits,
        )?
    } else {
        apply_replacements(&replacement_base_content, &matched_edits, 0)
    };

    if base_content == new_content {
        return Err(AgentError::Message(get_no_change_error(
            path,
            normalized_edits.len(),
        )));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

// ---------------------------------------------------------------------------
// Myers line-level diff (replaces jsdiff `Diff.diffLines`)
// ---------------------------------------------------------------------------

/// One raw diff sequence entry; each line keeps its trailing `\n` when present.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DiffEntry {
    Common(String),
    Removed(String),
    Added(String),
}

/// A grouped run of consecutive same-type entries (jsdiff `diffLines` parts).
#[derive(Clone, Debug)]
struct DiffPart {
    added: bool,
    removed: bool,
    lines: Vec<String>,
}

/// jsdiff's line tokenizer: `"a\nb\n"` → `["a\n", "b\n"]`, `""` → `[]`.
fn tokenize_lines(content: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            result.push(content[start..=i].to_string());
            start = i + 1;
        }
    }
    if start < content.len() {
        result.push(content[start..].to_string());
    }
    result
}

/// Myers diff producing removed-before-added ordering at each edit point
/// (matching jsdiff), in near-linear space.
///
/// The edit script equals the classic full-trace Myers O(ND) algorithm (and
/// jsdiff 8.x, whose forward path building uses the same "furthest-reaching,
/// deletion on tie" rule): the forward pass yields a deterministic sequence
/// of frontier rows `v_0..v_D`, and the backtrace from `(n, m)` re-derives
/// the script by applying that same tie rule to the rows.
///
/// A middle-snake divide-and-conquer on the edit graph cannot reproduce this
/// script byte for byte: the greedy path's middle snake depends on frontier
/// rows deeper than D/2, so any local overlap choice produces an
/// equivalent-but-different script (Myers's paper proves only minimality for
/// the D&C variant). Instead we divide the *backtrace depth*: frontier rows
/// are deterministic (the recurrence is order- and tie-independent), so the
/// row at depth `d` can be recomputed exactly from the row at depth `d/2`.
/// The backtrace keeps O(log D) checkpoint rows and recomputes each row
/// segment on demand; segments of at most [`BASE_SEGMENT`] steps store their
/// rows in a small bounded trace.
///
/// Memory: O(D log D) u32 entries — for the worst case D = n + m this is
/// ~(n+m) log(n+m), e.g. ~4 MB for two 4000-line fully-different inputs vs
/// ~1 GB for the classic O(D²) trace. Time: O(D² log D), the classic O(D²)
/// bound up to a log factor.
const BASE_SEGMENT: usize = 32;

fn myers_diff(a: &[String], b: &[String]) -> Vec<DiffEntry> {
    let n = a.len();
    let m = b.len();

    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return b.iter().map(|s| DiffEntry::Added(s.clone())).collect();
    }
    if m == 0 {
        return a.iter().map(|s| DiffEntry::Removed(s.clone())).collect();
    }

    // Pass 1: forward sweep without a trace — only the edit distance D and
    // the depth-0 row (common prefix) are kept.
    let (found_d, v0) = myers_find_distance(a, b);

    // The Myers forward pass always terminates with a solution.
    let d_total = match found_d {
        Some(d) => d,
        None => return Vec::new(),
    };

    // Pass 2: backtrace with checkpointed row recomputation. Entries are
    // pushed deepest-step-first and reversed at the end, like the classic
    // backtrace.
    let mut entries = Vec::new();
    let (x, y) = myers_backtrack_solve(a, b, &mut entries, 0, d_total, &v0, n, m);

    let mut x = x;
    let mut y = y;
    while x > 0 && y > 0 {
        entries.push(DiffEntry::Common(a[x - 1].clone()));
        x -= 1;
        y -= 1;
    }

    entries.reverse();
    entries
}

/// One Myers frontier row at depth `t`, padded on both ends:
/// `row[k + t + 1] = v_t[k]` for `k ∈ [-t, t]`, and the padding entries
/// (`k = ±(t+1)`) are 0. The padding mirrors the classic algorithm's single
/// v array, where the backtrace can read `v_{t-1}[k ± 1]` at `k = ±(t-1)`
/// and sees the array's untouched initial zero there.
fn myers_forward_row(a: &[String], b: &[String], prev: &[u32], t: usize) -> Vec<u32> {
    let n = a.len() as u32;
    let m = b.len();
    let mut row = vec![0u32; 2 * t + 3];

    let mut k = -(t as isize);
    while k <= t as isize {
        let x = if k == -(t as isize) {
            prev[(k + 1 + t as isize) as usize]
        } else if k == t as isize {
            prev[(k - 1 + t as isize) as usize] + 1
        } else {
            let vk_minus_1 = prev[(k - 1 + t as isize) as usize];
            let vk_plus_1 = prev[(k + 1 + t as isize) as usize];
            if vk_minus_1 < vk_plus_1 {
                vk_plus_1
            } else {
                vk_minus_1 + 1
            }
        };
        let mut x = x;
        let mut y = (x as isize - k) as usize;

        while x < n && y < m && a[x as usize] == b[y] {
            x += 1;
            y += 1;
        }

        row[(k + (t as isize + 1)) as usize] = x;
        k += 2;
    }

    row
}

/// Forward Myers sweep without a trace; returns the minimal edit distance `D`
/// and the padded depth-0 row (the common-prefix frontier).
fn myers_find_distance(a: &[String], b: &[String]) -> (Option<usize>, Vec<u32>) {
    let n = a.len() as u32;
    let m = b.len();
    let max_d = n as usize + m;
    let offset = max_d;
    let mut v = vec![0u32; 2 * max_d + 1];
    let mut found_d: Option<usize> = None;

    for d in 0..=max_d {
        let mut k = -(d as isize);
        while k <= d as isize {
            let x = if k == -(d as isize) {
                v[(k + 1 + offset as isize) as usize]
            } else if k == d as isize {
                v[(k - 1 + offset as isize) as usize] + 1
            } else {
                let vk_minus_1 = v[(k - 1 + offset as isize) as usize];
                let vk_plus_1 = v[(k + 1 + offset as isize) as usize];
                if vk_minus_1 < vk_plus_1 {
                    vk_plus_1
                } else {
                    vk_minus_1 + 1
                }
            };
            let mut x = x;
            let mut y = (x as isize - k) as usize;

            while x < n && y < m && a[x as usize] == b[y] {
                x += 1;
                y += 1;
            }

            v[(k + offset as isize) as usize] = x;

            if x >= n && y >= m {
                found_d = Some(d);
                break;
            }

            k += 2;
        }

        if found_d.is_some() {
            break;
        }
    }

    // Padded depth-0 row: the common-prefix frontier.
    let mut v0 = vec![0u32; 3];
    let mut x = 0usize;
    let mut y = 0usize;
    while x < n as usize && y < m && a[x] == b[y] {
        x += 1;
        y += 1;
    }
    v0[1] = x as u32;

    (found_d, v0)
}

/// Run the backtrace over the depth window `(a_depth, b_depth]`, recomputing
/// frontier rows from the checkpoint row `v_a` at depth `a_depth`.
///
/// `(sx, sy)` is the backtrace state at depth `b_depth`; the state at depth
/// `a_depth` is returned. Edit entries are pushed in reverse order
/// (deepest step first), matching the classic backtrace.
///
/// `a`/`b`/`entries` are the diff context; the rest is the recursion state —
/// a struct would hide the dataflow, so the parameters stay explicit.
#[allow(clippy::too_many_arguments)]
fn myers_backtrack_solve(
    a: &[String],
    b: &[String],
    entries: &mut Vec<DiffEntry>,
    a_depth: usize,
    b_depth: usize,
    v_a: &[u32],
    sx: usize,
    sy: usize,
) -> (usize, usize) {
    if b_depth - a_depth <= BASE_SEGMENT {
        // Recompute and store every row of the window (a bounded trace).
        let mut rows: Vec<Vec<u32>> = Vec::with_capacity(b_depth - a_depth);
        rows.push(v_a.to_vec());
        for t in (a_depth + 1)..b_depth {
            let prev = &rows[rows.len() - 1];
            let row = myers_forward_row(a, b, prev, t);
            rows.push(row);
        }

        let (mut x, mut y) = (sx, sy);
        for t in ((a_depth + 1)..=b_depth).rev() {
            let k = x as isize - y as isize;
            let t_i = t as isize;
            let vp = &rows[t - 1 - a_depth];

            let prev_k = if k == -t_i {
                k + 1
            } else if k == t_i {
                k - 1
            } else {
                let vk_minus_1 = vp[(k - 1 + t_i) as usize];
                let vk_plus_1 = vp[(k + 1 + t_i) as usize];
                if vk_minus_1 < vk_plus_1 { k + 1 } else { k - 1 }
            };

            let prev_x = vp[(prev_k + t_i) as usize] as usize;
            let prev_y = (prev_x as isize - prev_k) as usize;

            // Diagonal moves (common lines).
            while x > prev_x && y > prev_y {
                entries.push(DiffEntry::Common(a[x - 1].clone()));
                x -= 1;
                y -= 1;
            }

            // Edit step: exactly one of x or y differs from the previous state.
            if x == prev_x {
                entries.push(DiffEntry::Added(b[y - 1].clone()));
            } else {
                entries.push(DiffEntry::Removed(a[x - 1].clone()));
            }

            x = prev_x;
            y = prev_y;
        }
        return (x, y);
    }

    // Split the window: recompute the checkpoint row at the middle depth,
    // backtrace the deeper half first, then the shallower half.
    let mid = a_depth + (b_depth - a_depth) / 2;
    let mut cur = v_a.to_vec();
    for t in (a_depth + 1)..=mid {
        cur = myers_forward_row(a, b, &cur, t);
    }
    let (mx, my) = myers_backtrack_solve(a, b, entries, mid, b_depth, &cur, sx, sy);
    myers_backtrack_solve(a, b, entries, a_depth, mid, v_a, mx, my)
}

/// Group consecutive same-type diff entries into parts (jsdiff's
/// `{value, added, removed}` parts).
fn group_diff_parts(entries: &[DiffEntry]) -> Vec<DiffPart> {
    let mut parts: Vec<DiffPart> = Vec::new();
    for entry in entries {
        let (added, removed, line) = match entry {
            DiffEntry::Common(l) => (false, false, l),
            DiffEntry::Removed(l) => (false, true, l),
            DiffEntry::Added(l) => (true, false, l),
        };
        if let Some(last) = parts.last_mut()
            && last.added == added
            && last.removed == removed
        {
            last.lines.push(line.clone());
            continue;
        }
        parts.push(DiffPart {
            added,
            removed,
            lines: vec![line.clone()],
        });
    }
    parts
}

// ---------------------------------------------------------------------------
// Diff display generation (edit-diff.ts:377-500)
// ---------------------------------------------------------------------------

/// Result of `generate_diff_string` (edit-diff.ts:377-381).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStringResult {
    /// Display-oriented diff string with line numbers and context folding.
    pub diff: String,
    /// 1-indexed line number of the first changed line in the new file, if any.
    pub first_changed_line: Option<usize>,
}

/// `generateDiffString` (edit-diff.ts:377-500).
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffStringResult {
    let parts = {
        let old_tokens = tokenize_lines(old_content);
        let new_tokens = tokenize_lines(new_content);
        let entries = myers_diff(&old_tokens, &new_tokens);
        group_diff_parts(&entries)
    };
    let mut output: Vec<String> = Vec::new();

    // `String(...).length` on `split("\n")` counts (edit-diff.ts:385-388).
    let old_lines_count = old_content.split('\n').count();
    let new_lines_count = new_content.split('\n').count();
    let max_line_num = old_lines_count.max(new_lines_count);
    let line_num_width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (i, part) in parts.iter().enumerate() {
        // `part.value.split("\n")` with a trailing empty popped
        // (edit-diff.ts:397-400).
        let raw: Vec<&str> = part
            .lines
            .iter()
            .map(|line| line.strip_suffix('\n').unwrap_or(line))
            .collect();

        if part.added || part.removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }
            for line in &raw {
                if part.added {
                    let line_num = format!("{new_line_num:>width$}", width = line_num_width);
                    output.push(format!("+{line_num} {line}"));
                    new_line_num += 1;
                } else {
                    let line_num = format!("{old_line_num:>width$}", width = line_num_width);
                    output.push(format!("-{line_num} {line}"));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            // Context lines — only show a few around changes (edit-diff.ts:422-494).
            let next_part_is_change =
                i + 1 < parts.len() && (parts[i + 1].added || parts[i + 1].removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in &raw {
                        let line_num = format!("{old_line_num:>width$}", width = line_num_width);
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading_lines = &raw[..context_lines];
                    let trailing_lines = &raw[raw.len() - context_lines..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        let line_num = format!("{old_line_num:>width$}", width = line_num_width);
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }

                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;

                    for line in trailing_lines {
                        let line_num = format!("{old_line_num:>width$}", width = line_num_width);
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown_lines = &raw[..raw.len().min(context_lines)];
                let skipped_lines = raw.len() - shown_lines.len();

                for line in shown_lines {
                    let line_num = format!("{old_line_num:>width$}", width = line_num_width);
                    output.push(format!(" {line_num} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }

                if skipped_lines > 0 {
                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
            } else if has_trailing_change {
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }

                for line in &raw[skipped_lines..] {
                    let line_num = format!("{old_line_num:>width$}", width = line_num_width);
                    output.push(format!(" {line_num} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                // Skip these context lines entirely (edit-diff.ts:489-493).
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    DiffStringResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

// ---------------------------------------------------------------------------
// Unified patch generation (edit-diff.ts:366-371, jsdiff createTwoFilesPatch)
// ---------------------------------------------------------------------------

/// `generateUnifiedPatch` (edit-diff.ts:366-371) — jsdiff `createTwoFilesPatch`
/// with `FILE_HEADERS_ONLY`: `--- path` / `+++ path`, `@@ -a,b +c,d @@` hunks
/// (4 context lines by default), `\ No newline at end of file` markers, and a
/// trailing newline.
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let old_tokens = tokenize_lines(old_content);
    let new_tokens = tokenize_lines(new_content);
    let entries = myers_diff(&old_tokens, &new_tokens);

    // Include changed entries plus `context_lines` of context on each side.
    let n = entries.len();
    let mut include = vec![false; n];
    for (i, entry) in entries.iter().enumerate() {
        if matches!(entry, DiffEntry::Removed(_) | DiffEntry::Added(_)) {
            let start = i.saturating_sub(context_lines);
            let end = (i + context_lines + 1).min(n);
            include[start..end].fill(true);
        }
    }

    // Group included entries into hunks (consecutive runs).
    let mut hunk_ranges: Vec<(usize, usize)> = Vec::new(); // [start, end)
    {
        let mut i = 0;
        while i < n {
            if !include[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < n && include[i] {
                i += 1;
            }
            hunk_ranges.push((start, i));
        }
    }

    let mut output: Vec<String> = Vec::new();
    output.push(format!("--- {path}"));
    output.push(format!("+++ {path}"));

    for &(hunk_start, hunk_end) in &hunk_ranges {
        // Walk all entries up to hunk_start to find old/new line numbers.
        let mut ol = 1usize;
        let mut nl = 1usize;
        for entry in &entries[..hunk_start] {
            match entry {
                DiffEntry::Common(_) => {
                    ol += 1;
                    nl += 1;
                }
                DiffEntry::Removed(_) => ol += 1,
                DiffEntry::Added(_) => nl += 1,
            }
        }

        // Count old/new lines in the hunk.
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        for entry in &entries[hunk_start..hunk_end] {
            match entry {
                DiffEntry::Common(_) => {
                    old_count += 1;
                    new_count += 1;
                }
                DiffEntry::Removed(_) => old_count += 1,
                DiffEntry::Added(_) => new_count += 1,
            }
        }

        // Hunk header. When count is 0, start is the line before the change.
        let old_start = if old_count == 0 {
            ol.saturating_sub(1)
        } else {
            ol
        };
        let new_start = if new_count == 0 {
            nl.saturating_sub(1)
        } else {
            nl
        };
        output.push(format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
        ));

        // Hunk lines.
        for entry in &entries[hunk_start..hunk_end] {
            match entry {
                DiffEntry::Common(line) => {
                    let l = line.strip_suffix('\n').unwrap_or(line);
                    output.push(format!(" {l}"));
                    if !line.ends_with('\n') {
                        output.push("\\ No newline at end of file".to_string());
                    }
                }
                DiffEntry::Removed(line) => {
                    let l = line.strip_suffix('\n').unwrap_or(line);
                    output.push(format!("-{l}"));
                    if !line.ends_with('\n') {
                        output.push("\\ No newline at end of file".to_string());
                    }
                }
                DiffEntry::Added(line) => {
                    let l = line.strip_suffix('\n').unwrap_or(line);
                    output.push(format!("+{l}"));
                    if !line.ends_with('\n') {
                        output.push("\\ No newline at end of file".to_string());
                    }
                }
            }
        }
    }

    // jsdiff always ends the patch with a trailing newline.
    output.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden cases captured from jsdiff 8.x (`Diff.createTwoFilesPatch` with
    /// `FILE_HEADERS_ONLY` and `context: 4`, and `Diff.diffLines` fed through
    /// the upstream `generateDiffString` algorithm) — the reference behavior
    /// the hand-rolled Myers port must reproduce byte for byte.
    struct Case {
        path: &'static str,
        old_content: &'static str,
        new_content: &'static str,
        patch: &'static str,
        diff: &'static str,
        first_changed_line: Option<usize>,
    }

    const CASES: &[Case] = &[
        Case {
            path: "edit.txt",
            old_content: "alpha\nbeta\ngamma\ndelta\n",
            new_content: "ALPHA\nbeta\nGAMMA\ndelta\n",
            patch: "--- edit.txt\n+++ edit.txt\n@@ -1,4 +1,4 @@\n-alpha\n+ALPHA\n beta\n-gamma\n+GAMMA\n delta\n",
            diff: "-1 alpha\n+1 ALPHA\n 2 beta\n-3 gamma\n+3 GAMMA\n 4 delta",
            first_changed_line: Some(1),
        },
        Case {
            path: "a",
            old_content: "one\ntwo\nthree\n",
            new_content: "ONE\nTWO\nTHREE\n",
            patch: "--- a\n+++ a\n@@ -1,3 +1,3 @@\n-one\n-two\n-three\n+ONE\n+TWO\n+THREE\n",
            diff: "-1 one\n-2 two\n-3 three\n+1 ONE\n+2 TWO\n+3 THREE",
            first_changed_line: Some(1),
        },
        Case {
            path: "b",
            old_content: "x\ny\nz\n",
            new_content: "x\nz\n",
            patch: "--- b\n+++ b\n@@ -1,3 +1,2 @@\n x\n-y\n z\n",
            diff: " 1 x\n-2 y\n 3 z",
            first_changed_line: Some(2),
        },
        // Context folding on both sides of a change (20-line context block).
        Case {
            path: "c",
            old_content: "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np\nq\nr\ns\nt\nu\nv\nw\nx\ny\nz\n",
            new_content: "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np\nq\nr\ns\nt\nZ\nu\nv\nw\nx\ny\nz\n",
            patch: "--- c\n+++ c\n@@ -17,8 +17,9 @@\n q\n r\n s\n t\n+Z\n u\n v\n w\n x\n",
            diff: "    ...\n 17 q\n 18 r\n 19 s\n 20 t\n+21 Z\n 21 u\n 22 v\n 23 w\n 24 x\n    ...",
            first_changed_line: Some(21),
        },
        // Pure insertion at the start (`-0,0` hunk).
        Case {
            path: "d",
            old_content: "",
            new_content: "new1\nnew2\n",
            patch: "--- d\n+++ d\n@@ -0,0 +1,2 @@\n+new1\n+new2\n",
            diff: "+1 new1\n+2 new2",
            first_changed_line: Some(1),
        },
        // Pure deletion at the start (`+0,0` hunk).
        Case {
            path: "e",
            old_content: "old1\nold2\n",
            new_content: "",
            patch: "--- e\n+++ e\n@@ -1,2 +0,0 @@\n-old1\n-old2\n",
            diff: "-1 old1\n-2 old2",
            first_changed_line: Some(1),
        },
        // Missing trailing newline markers.
        Case {
            path: "f",
            old_content: "no trailing",
            new_content: "no trailing edited",
            patch: "--- f\n+++ f\n@@ -1,1 +1,1 @@\n-no trailing\n\\ No newline at end of file\n+no trailing edited\n\\ No newline at end of file\n",
            diff: "-1 no trailing\n+1 no trailing edited",
            first_changed_line: Some(1),
        },
        Case {
            path: "g",
            old_content: "a\nb\nc\n",
            new_content: "a\nb\nc\nd\n",
            patch: "--- g\n+++ g\n@@ -1,3 +1,4 @@\n a\n b\n c\n+d\n",
            diff: " 1 a\n 2 b\n 3 c\n+4 d",
            first_changed_line: Some(4),
        },
        Case {
            path: "h",
            old_content: "a\nb\nc\nd\n",
            new_content: "a\nb\nc\n",
            patch: "--- h\n+++ h\n@@ -1,4 +1,3 @@\n a\n b\n c\n-d\n",
            diff: " 1 a\n 2 b\n 3 c\n-4 d",
            first_changed_line: Some(4),
        },
        // No changes: header-only patch, empty diff.
        Case {
            path: "i",
            old_content: "same\n",
            new_content: "same\n",
            patch: "--- i\n+++ i\n",
            diff: "",
            first_changed_line: None,
        },
        // Blank-line replacement (empty display line).
        Case {
            path: "j",
            old_content: "x\n\ny\n",
            new_content: "x\nz\ny\n",
            patch: "--- j\n+++ j\n@@ -1,3 +1,3 @@\n x\n-\n+z\n y\n",
            diff: " 1 x\n-2 \n+2 z\n 3 y",
            first_changed_line: Some(2),
        },
    ];

    #[test]
    fn unified_patch_matches_jsdiff_golden_output() {
        for case in CASES {
            let patch = generate_unified_patch(case.path, case.old_content, case.new_content, 4);
            assert_eq!(&patch, case.patch, "patch mismatch for case {}", case.path);
        }
    }

    #[test]
    fn diff_string_matches_jsdiff_golden_output() {
        for case in CASES {
            let result = generate_diff_string(case.old_content, case.new_content, 4);
            assert_eq!(
                &result.diff, case.diff,
                "display diff mismatch for case {}",
                case.path
            );
            assert_eq!(
                result.first_changed_line, case.first_changed_line,
                "firstChangedLine mismatch for case {}",
                case.path
            );
        }
    }

    #[test]
    fn generated_patches_roundtrip_through_the_test_applier() {
        for case in CASES {
            let patch = generate_unified_patch(case.path, case.old_content, case.new_content, 4);
            let applied =
                crate::harness::tools::test_helpers::apply_unified_patch(case.old_content, &patch);
            assert_eq!(
                &applied, case.new_content,
                "patch roundtrip mismatch for case {}",
                case.path
            );
        }
    }

    #[test]
    fn fuzzy_matching_normalizes_quotes_dashes_and_spaces() {
        // `normalizeForFuzzyMatch` (edit-diff.ts:30-51).
        let content = "\u{201C}smart\u{201D} \u{2013} en-dash \u{00A0}nb\u{3000}sp\u{202F}";
        let normalized = normalize_for_fuzzy_match(content);
        // The trailing U+202F is trimmed by the per-line `trim_end` before the
        // space replacement runs (edit-diff.ts:35-37), so it does not survive.
        assert_eq!(normalized, "\"smart\" - en-dash  nb sp");
        // NFKC: full-width forms collapse.
        assert_eq!(normalize_for_fuzzy_match("\u{FF21}BC"), "ABC");
        // Trailing whitespace stripped per line.
        assert_eq!(normalize_for_fuzzy_match("a  \nb\t"), "a\nb");
    }

    #[test]
    fn applies_fuzzy_edits_preserving_unchanged_lines() {
        // `applyEditsToNormalizedContent` with a fuzzy-only match (curly
        // apostrophe in the oldText): the change must be overlaid onto the
        // original without touching other lines (edit-diff.ts:354-356).
        let content = "line one\nit's here\nline three\n";
        let edits = vec![Edit {
            old_text: "it\u{2019}s here".to_string(),
            new_text: "changed".to_string(),
        }];
        let result = apply_edits_to_normalized_content(content, &edits, "f.txt").unwrap();
        assert_eq!(result.new_content, "line one\nchanged\nline three\n");
        assert_eq!(result.base_content, content);
    }

    #[test]
    fn rejects_empty_old_text_and_no_change() {
        let content = "abc\n";
        let empty = apply_edits_to_normalized_content(
            content,
            &[Edit {
                old_text: String::new(),
                new_text: "x".to_string(),
            }],
            "f.txt",
        )
        .unwrap_err();
        assert!(empty.to_string().contains("oldText must not be empty"));

        let no_change = apply_edits_to_normalized_content(
            content,
            &[Edit {
                old_text: "abc".to_string(),
                new_text: "abc".to_string(),
            }],
            "f.txt",
        )
        .unwrap_err();
        assert!(no_change.to_string().contains("No changes made to f.txt"));
    }

    // -----------------------------------------------------------------------
    // Linear-space `myers_diff` behavior lock
    // -----------------------------------------------------------------------

    /// The classic full-trace Myers O(ND) algorithm — the behavioral
    /// reference the linear-space `myers_diff` must reproduce byte for byte.
    /// (Kept in the tests; jsdiff-8.x golden agreement is covered by the
    /// `unified_patch_matches_jsdiff_golden_output` and
    /// `diff_string_matches_jsdiff_golden_output` tests above.)
    fn myers_diff_reference(a: &[String], b: &[String]) -> Vec<DiffEntry> {
        let n = a.len();
        let m = b.len();

        if n == 0 && m == 0 {
            return Vec::new();
        }
        if n == 0 {
            return b.iter().map(|s| DiffEntry::Added(s.clone())).collect();
        }
        if m == 0 {
            return a.iter().map(|s| DiffEntry::Removed(s.clone())).collect();
        }

        let max_d = n + m;
        let offset = max_d;
        let v_len = 2 * max_d + 1;
        let mut v = vec![0usize; v_len];
        let mut trace: Vec<Vec<usize>> = Vec::with_capacity(max_d + 1);

        let mut found_d: Option<usize> = None;

        for d in 0..=max_d {
            trace.push(v.clone());

            let mut k = -(d as isize);
            while k <= d as isize {
                let x = if k == -(d as isize) {
                    v[(k + 1 + offset as isize) as usize]
                } else if k == d as isize {
                    v[(k - 1 + offset as isize) as usize] + 1
                } else {
                    let vk_minus_1 = v[(k - 1 + offset as isize) as usize];
                    let vk_plus_1 = v[(k + 1 + offset as isize) as usize];
                    if vk_minus_1 < vk_plus_1 {
                        vk_plus_1
                    } else {
                        vk_minus_1 + 1
                    }
                };
                let mut x = x;
                let mut y = (x as isize - k) as usize;

                while x < n && y < m && a[x] == b[y] {
                    x += 1;
                    y += 1;
                }

                v[(k + offset as isize) as usize] = x;

                if x >= n && y >= m {
                    found_d = Some(d);
                    break;
                }

                k += 2;
            }

            if found_d.is_some() {
                break;
            }
        }

        let d_total = match found_d {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut entries = Vec::new();
        let mut x = n;
        let mut y = m;

        for d in (1..=d_total).rev() {
            let vp = &trace[d];
            let k = x as isize - y as isize;

            let prev_k = if k == -(d as isize) {
                k + 1
            } else if k == d as isize {
                k - 1
            } else {
                let vk_minus_1 = vp[((k - 1) + offset as isize) as usize];
                let vk_plus_1 = vp[((k + 1) + offset as isize) as usize];
                if vk_minus_1 < vk_plus_1 { k + 1 } else { k - 1 }
            };

            let prev_x = vp[(prev_k + offset as isize) as usize];
            let prev_y = (prev_x as isize - prev_k) as usize;

            while x > prev_x && y > prev_y {
                entries.push(DiffEntry::Common(a[x - 1].clone()));
                x -= 1;
                y -= 1;
            }

            if x == prev_x {
                entries.push(DiffEntry::Added(b[y - 1].clone()));
            } else {
                entries.push(DiffEntry::Removed(a[x - 1].clone()));
            }

            x = prev_x;
            y = prev_y;
        }

        while x > 0 && y > 0 {
            entries.push(DiffEntry::Common(a[x - 1].clone()));
            x -= 1;
            y -= 1;
        }

        entries.reverse();
        entries
    }

    /// All strings over the given alphabet up to `max_len`, length-first.
    fn enumerate_strings(alphabet: &[&str], max_len: usize) -> Vec<Vec<String>> {
        fn build(
            alphabet: &[&str],
            depth: usize,
            max_len: usize,
            prefix: &mut Vec<String>,
            out: &mut Vec<Vec<String>>,
        ) {
            out.push(prefix.clone());
            if depth == max_len {
                return;
            }
            for symbol in alphabet {
                prefix.push(symbol.to_string());
                build(alphabet, depth + 1, max_len, prefix, out);
                prefix.pop();
            }
        }
        let mut out = Vec::new();
        build(alphabet, 0, max_len, &mut Vec::new(), &mut out);
        out
    }

    #[test]
    fn myers_diff_matches_reference_exhaustively() {
        // Every pair of strings over a 3-symbol alphabet with up to 5 lines:
        // 364 × 364 = 132,496 pairs.
        let strings = enumerate_strings(&["a", "b", "c"], 5);
        for old in &strings {
            for new in &strings {
                let expected = myers_diff_reference(old, new);
                let got = myers_diff(old, new);
                assert_eq!(got, expected, "old={old:?} new={new:?}");
            }
        }
    }

    #[test]
    fn myers_diff_matches_reference_on_random_inputs() {
        // Deterministic LCG so failures are reproducible.
        let mut state = 0x5eed_2026_0806_u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _ in 0..2000 {
            let n = (next() % 80) as usize;
            let m = (next() % 80) as usize;
            let alpha = 2 + next() % 9;
            let old: Vec<String> = (0..n).map(|_| (next() % alpha).to_string()).collect();
            let new: Vec<String> = (0..m).map(|_| (next() % alpha).to_string()).collect();
            let expected = myers_diff_reference(&old, &new);
            let got = myers_diff(&old, &new);
            assert_eq!(got, expected, "old={old:?} new={new:?}");
        }
    }

    #[test]
    fn myers_diff_matches_reference_on_large_inputs() {
        // 2×1000 fully-different lines (D = 2000): exercises the depth
        // checkpointing across multiple splits. The reference full-trace
        // peaks at ~64 MB here; the linear-space variant stays at a few MB.
        let old: Vec<String> = (0..1000).map(|i| format!("old line {i}")).collect();
        let new: Vec<String> = (0..1000).map(|i| format!("new line {i}")).collect();
        let expected = myers_diff_reference(&old, &new);
        let got = myers_diff(&old, &new);
        assert_eq!(got, expected);
    }

    #[test]
    fn myers_diff_handles_2x4000_all_different_lines_in_near_linear_space() {
        // The worst case that made the old O(D²) trace peak at ~1 GB
        // (2×4000 fully-different lines: D = 8000). The checkpointed variant
        // runs in O(D log D) space (~4 MB) and produces the same script:
        // all deletions, then all insertions (removed-before-added ordering).
        let old: Vec<String> = (0..4000).map(|i| format!("old line {i}")).collect();
        let new: Vec<String> = (0..4000).map(|i| format!("new line {i}")).collect();
        let entries = myers_diff(&old, &new);
        assert_eq!(entries.len(), 8000);
        for (i, entry) in entries.iter().enumerate() {
            let expected = if i < 4000 {
                DiffEntry::Removed(format!("old line {i}"))
            } else {
                DiffEntry::Added(format!("new line {}", i - 4000))
            };
            assert_eq!(entry, &expected, "entry {i}");
        }

        // The derived outputs stay consistent on the same input.
        let old_text = old.join("\n");
        let new_text = new.join("\n");
        let patch = generate_unified_patch("big.txt", &old_text, &new_text, 4);
        assert!(patch.starts_with("--- big.txt\n+++ big.txt\n@@ -1,4000 +1,4000 @@\n-"));
        let diff = generate_diff_string(&old_text, &new_text, 4);
        assert_eq!(diff.first_changed_line, Some(1));
        assert!(
            diff.diff
                .starts_with("-   1 old line 0\n-   2 old line 1\n")
        );
    }
}
