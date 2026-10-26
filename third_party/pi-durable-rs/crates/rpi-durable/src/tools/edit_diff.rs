//! Shared diff computation, fuzzy matching and text-replacement utilities for
//! the durable edit tool (port of `packages/durable/src/tools/edit-diff.ts`
//! @ `abe508e1b`).
//!
//! The upstream durable module is the same core as the coding-agent one minus
//! `computeEditsDiff`; the jsdiff-compatible Myers diff and unified-patch
//! writer are ported from the in-repo coding-agent port
//! (`crates/rpi/src/tools/edit_diff.rs` @ 0.82.1) so the byte format stays
//! exactly jsdiff's.
//!
//! # Index space note
//!
//! JavaScript `String.indexOf` / `String.length` operate on UTF-16 code units.
//! Rust string operations operate on bytes (`usize`). Throughout this module we
//! use byte indices for matching and slicing. Because matching and slicing stay
//! within the same index space (byte offsets into a `&str`), the resulting text
//! output is identical to the upstream JavaScript implementation.

use unicode_normalization::UnicodeNormalization;

// ---------------------------------------------------------------------------
// Line-ending helpers (edit-diff.ts:7-21)
// ---------------------------------------------------------------------------

/// Detect the dominant line ending style.
///
/// Port of `detectLineEnding` (edit-diff.ts:7-13). Finds the first `\r\n` and
/// the first `\n`. If neither exists, or `\r\n` does not exist, returns `"\n"`.
/// If `\r\n` appears before the first lone `\n`, returns `"\r\n"`.
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

/// Normalise `\r\n` → `\n`, then remaining `\r` → `\n`.
///
/// Port of `normalizeToLF` (edit-diff.ts:15-17).
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore line endings: if `ending` is `"\r\n"`, replace all `\n` with `\r\n`.
///
/// Port of `restoreLineEndings` (edit-diff.ts:19-21).
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

// ---------------------------------------------------------------------------
// BOM (edit-diff.ts:244-246)
// ---------------------------------------------------------------------------

/// Strip a leading UTF-8 BOM (`\u{FEFF}`) if present.
///
/// Returns `(bom, text_without_bom)`.
///
/// Port of `stripBom` (edit-diff.ts:244-246).
pub fn strip_bom(content: &str) -> (String, String) {
    if let Some(rest) = content.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}".to_string(), rest.to_string())
    } else {
        (String::new(), content.to_string())
    }
}

// ---------------------------------------------------------------------------
// Fuzzy normalisation (edit-diff.ts:30-51)
// ---------------------------------------------------------------------------

/// Normalise text for fuzzy matching.
///
/// Applies progressive transformations in order (edit-diff.ts:30-51):
/// 1. NFKC Unicode normalisation (full-width → half-width, compatibility forms)
/// 2. Per-line trailing whitespace stripping (`trim_end`)
/// 3. Smart single quotes `\u{2018}\u{2019}\u{201A}\u{201B}` → `'`
/// 4. Smart double quotes `\u{201C}\u{201D}\u{201E}\u{201F}` → `"`
/// 5. Unicode dashes/hyphens `\u{2010}`-`\u{2015}`, `\u{2212}` → `-`
/// 6. Special spaces `\u{00A0}`, `\u{2002}`-`\u{200A}`, `\u{202F}`, `\u{205F}`,
///    `\u{3000}` → regular space
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    // 1. NFKC
    let nfkc: String = text.nfkc().collect();

    // 2. Trim trailing whitespace per line
    let trimmed: String = nfkc
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");

    // 3-6. Character-level replacements
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
            '\u{00A0}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            '\u{2002}'..='\u{200A}' => ' ',
            _ => c,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Line splitting helpers (edit-diff.ts:53-78)
// ---------------------------------------------------------------------------

/// Split content into lines, each INCLUDING its trailing `\n` if present.
///
/// Equivalent to the JS regex `/[^\n]*\n|[^\n]+/g` (edit-diff.ts:53-55).
/// Returns string slices (no allocation).
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

/// Byte-offset span of a line within the original content.
#[derive(Clone, Copy, Debug)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// Compute byte-offset spans for each line in `content`.
///
/// Port of `getLineSpans` (edit-diff.ts:71-78).
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
// Replacement types
// ---------------------------------------------------------------------------

/// A single targeted text replacement (`Edit`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Exact text to find in the file.
    pub old_text: String,
    /// Replacement text.
    pub new_text: String,
}

/// Result of `apply_edits_to_normalized_content`.
#[derive(Clone, Debug)]
pub struct AppliedEditsResult {
    /// The original (LF-normalized) content before edits.
    pub base_content: String,
    /// The content after edits have been applied.
    pub new_content: String,
}

/// A matched edit with its position in the content.
#[derive(Clone, Debug)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

// ---------------------------------------------------------------------------
// Line-range computation (edit-diff.ts:80-105)
// ---------------------------------------------------------------------------

/// Find the range of lines (exclusive end) touched by a replacement.
///
/// Port of `getReplacementLineRange` (edit-diff.ts:80-105).
fn get_replacement_line_range(
    lines: &[LineSpan],
    match_index: usize,
    match_length: usize,
) -> Option<(usize, usize)> {
    let replacement_start = match_index;
    let replacement_end = match_index + match_length;

    let mut start_line = 0;
    let mut found = false;
    for (i, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = i;
            found = true;
            break;
        }
    }
    if !found {
        return None;
    }

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    // end_line is the first line whose end >= replacement_end, or lines.len()
    Some((start_line, end_line + 1))
}

// ---------------------------------------------------------------------------
// Apply replacements (edit-diff.ts:107-169)
// ---------------------------------------------------------------------------

/// Apply replacements directly in the given content (reverse order for stability).
///
/// Port of `applyReplacements` (edit-diff.ts:107-116). `offset` is subtracted
/// from each `match_index` because the content may be a slice of a larger string.
fn apply_replacements(content: &str, replacements: &[MatchedEdit], offset: usize) -> String {
    let mut result = content.to_string();
    // Process in reverse order so earlier offsets stay valid.
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index.saturating_sub(offset);
        let before = &result[..match_index];
        let after = &result[match_index + replacement.match_length..];
        result = format!("{before}{}{after}", replacement.new_text);
    }
    result
}

/// Apply replacements matched against `base_content` to `original_content`,
/// preserving unchanged line blocks from the original.
///
/// Port of `applyReplacementsPreservingUnchangedLines` (edit-diff.ts:128-169).
/// Each replacement is widened to the lines it touches; touched lines are
/// rewritten from the normalized base, all other lines are copied verbatim
/// from the original. This prevents duplicate normalized lines from being
/// aligned to the wrong occurrence.
fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    matched_edits: &[MatchedEdit],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    // The number of lines must match because base_content is a normalized view
    // of original_content (same line structure, different character content).
    // If they differ we fall back to a simple string concatenation of the base.
    if original_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .to_string(),
        );
    }

    // Build groups of adjacent/overlapping replacements.
    let mut sorted: Vec<&MatchedEdit> = matched_edits.iter().collect();
    sorted.sort_by_key(|e| e.match_index);

    #[derive(Clone)]
    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<MatchedEdit>,
    }

    let mut groups: Vec<Group> = Vec::new();
    for replacement in &sorted {
        let range = get_replacement_line_range(
            &base_lines,
            replacement.match_index,
            replacement.match_length,
        );
        let Some((start_line, end_line)) = range else {
            continue;
        };
        if let Some(current) = groups.last_mut()
            && start_line < current.end_line
        {
            current.end_line = current.end_line.max(end_line);
            current.replacements.push((*replacement).clone());
            continue;
        }
        groups.push(Group {
            start_line,
            end_line,
            replacements: vec![(**replacement).clone()],
        });
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();

    for group in &groups {
        // Copy original lines before this group.
        for &line in &original_lines[original_line_index..group.start_line] {
            result.push_str(line);
        }

        // Apply replacements within the group slice of base_content.
        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        let slice = &base_content[group_start_offset..group_end_offset];
        result.push_str(&apply_replacements(
            slice,
            &group.replacements,
            group_start_offset,
        ));

        original_line_index = group.end_line;
    }

    // Copy remaining original lines.
    for &line in &original_lines[original_line_index..] {
        result.push_str(line);
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Fuzzy matching (edit-diff.ts:203-252)
// ---------------------------------------------------------------------------

/// Result of a fuzzy text search.
struct FuzzyMatchResult {
    found: bool,
    index: usize,
    match_length: usize,
    used_fuzzy_match: bool,
}

/// Find `old_text` in `content`, trying exact match first, then fuzzy match.
///
/// Port of `fuzzyFindText` (edit-diff.ts:203-241).
fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    // Try exact match first.
    if let Some(index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
        };
    }

    // Fuzzy fallback.
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

/// Count occurrences of `old_text` in `content`, always in fuzzy-normalized space.
///
/// Port of `countOccurrences` (edit-diff.ts:248-252). Even if an exact match
/// succeeded, counting happens in fuzzy space so that fuzzy-normalized
/// duplicates are detected.
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

/// Apply one or more exact-text replacements to LF-normalized content.
///
/// All edits are matched against the same original content. Replacements are
/// then applied so offsets remain stable. If any edit needs fuzzy matching, the
/// entire operation runs in fuzzy-normalized content space and then overlays
/// those line-level changes onto the original content so unchanged line blocks
/// keep their original bytes.
///
/// Port of `applyEditsToNormalizedContent` (edit-diff.ts:301-363).
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    // Normalise each edit's old/new text to LF.
    let normalized_edits: Vec<(String, String, usize)> = edits
        .iter()
        .enumerate()
        .map(|(index, edit)| {
            (
                normalize_to_lf(&edit.old_text),
                normalize_to_lf(&edit.new_text),
                index,
            )
        })
        .collect();

    // Empty oldText check.
    for (i, (old_text, _, _)) in normalized_edits.iter().enumerate() {
        if old_text.is_empty() {
            return Err(get_empty_old_text_error(path, i, normalized_edits.len()));
        }
    }

    // Initial matching pass: determine if any edit needs fuzzy matching.
    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|(old_text, _, _)| fuzzy_find_text(normalized_content, old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|m| m.used_fuzzy_match);

    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    // Per-edit matching and validation.
    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (i, (old_text, new_text, _)) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, old_text);
        if !match_result.found {
            return Err(get_not_found_error(path, i, normalized_edits.len()));
        }

        let occurrences = count_occurrences(&replacement_base_content, old_text);
        if occurrences > 1 {
            return Err(get_duplicate_error(
                path,
                i,
                normalized_edits.len(),
                occurrences,
            ));
        }

        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: new_text.clone(),
        });
    }

    // Overlap detection (sorted by match_index).
    matched_edits.sort_by_key(|e| e.match_index);
    for window in matched_edits.windows(2) {
        let previous = &window[0];
        let current = &window[1];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index, path
            ));
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
        return Err(get_no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

// ---------------------------------------------------------------------------
// Myers line-level diff (replaces jsdiff Diff.diffLines)
// ---------------------------------------------------------------------------

/// A single entry in the raw diff sequence.
#[derive(Clone, Debug)]
enum DiffEntry {
    /// Line present in both old and new (includes trailing `\n` if present).
    Common(String),
    /// Line present only in old.
    Removed(String),
    /// Line present only in new.
    Added(String),
}

/// A grouped run of consecutive same-type entries.
#[derive(Clone, Debug)]
struct DiffPart {
    /// `true` for added, `false` otherwise.
    added: bool,
    /// `true` for removed, `false` otherwise.
    removed: bool,
    /// Lines in this part (each WITH trailing `\n` if present in source).
    lines: Vec<String>,
}

/// Split content into line tokens, each including its trailing `\n` if present.
///
/// Matches jsdiff's line tokenizer: `"a\nb\n"` → `["a\n", "b\n"]`,
/// `"a\nb"` → `["a\n", "b"]`, `""` → `[]`.
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

/// Compute a line-level diff using the Myers O(ND) algorithm.
///
/// Produces a sequence of `DiffEntry` values in forward order. Removed entries
/// precede added entries at the same edit point (matching jsdiff ordering).
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

    let max_d = n + m;
    let offset = max_d; // for 1-indexed-to-0-indexed conversion of k values
    let v_len = 2 * max_d + 1;
    let mut v = vec![0usize; v_len];
    let mut trace: Vec<Vec<usize>> = Vec::with_capacity(max_d + 1);

    let mut found_d: Option<usize> = None;

    for d in 0..=max_d {
        trace.push(v.clone());

        let mut k = -(d as isize);
        while k <= d as isize {
            let mut x: usize = if k == -(d as isize) {
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

            let mut y = (x as isize - k) as usize;

            // Follow diagonal (common prefix).
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

    let d_total = found_d.expect("Myers algorithm must find a solution");

    // Backtrack to produce the edit script.
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

        // Diagonal moves (common lines).
        while x > prev_x && y > prev_y {
            entries.push(DiffEntry::Common(a[x - 1].clone()));
            x -= 1;
            y -= 1;
        }

        // Edit step: exactly one of x or y differs from the previous state.
        if x == prev_x {
            // Insert (came from k+1): push the added line from b.
            entries.push(DiffEntry::Added(b[y - 1].clone()));
        } else {
            // Delete (came from k-1): push the removed line from a.
            entries.push(DiffEntry::Removed(a[x - 1].clone()));
        }

        x = prev_x;
        y = prev_y;
    }

    // Remaining diagonal at d=0.
    while x > 0 && y > 0 {
        entries.push(DiffEntry::Common(a[x - 1].clone()));
        x -= 1;
        y -= 1;
    }

    entries.reverse();
    entries
}

/// Group consecutive same-type diff entries into parts.
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

/// Result of `generate_diff_string`.
#[derive(Clone, Debug)]
pub struct DiffResult {
    /// Display-oriented diff string with line numbers and context folding.
    pub diff: String,
    /// 1-indexed line number of the first changed line in the new file, if any.
    pub first_changed_line: Option<usize>,
}

/// Generate a display-oriented diff string with line numbers and context.
///
/// Port of `generateDiffString` (edit-diff.ts:377-500). Uses a Myers line-level
/// diff instead of jsdiff's `Diff.diffLines`, producing the same output format.
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffResult {
    let parts = {
        let old_tokens = tokenize_lines(old_content);
        let new_tokens = tokenize_lines(new_content);
        let entries = myers_diff(&old_tokens, &new_tokens);
        group_diff_parts(&entries)
    };

    let mut output: Vec<String> = Vec::new();

    // Compute line-number width from split("\n") lengths (matches upstream).
    let old_lines_count = old_content.split('\n').count();
    let new_lines_count = new_content.split('\n').count();
    let max_line_num = old_lines_count.max(new_lines_count);
    let line_num_width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (i, part) in parts.iter().enumerate() {
        // Extract display lines: each token includes trailing '\n' if present.
        // Stripping it matches jsdiff's `part.value.split("\n")` with trailing
        // empty popped.
        let raw: Vec<&str> = part
            .lines
            .iter()
            .map(|l| l.strip_suffix('\n').unwrap_or(l))
            .collect();

        if part.added || part.removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }
            for line in &raw {
                if part.added {
                    output.push(format!(
                        "+{:>width$} {}",
                        new_line_num,
                        line,
                        width = line_num_width
                    ));
                    new_line_num += 1;
                } else {
                    output.push(format!(
                        "-{:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            // Context lines with folding.
            let next_part_is_change =
                i + 1 < parts.len() && (parts[i + 1].added || parts[i + 1].removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in &raw {
                        output.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading = &raw[..context_lines];
                    let trailing = &raw[raw.len() - context_lines..];
                    let skipped = raw.len() - leading.len() - trailing.len();

                    for line in leading {
                        output.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }

                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped;
                    new_line_num += skipped;

                    for line in trailing {
                        output.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown = &raw[..raw.len().min(context_lines)];
                let skipped = raw.len() - shown.len();

                for line in shown {
                    output.push(format!(
                        " {:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }

                if skipped > 0 {
                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
            } else if has_trailing_change {
                let skipped = raw.len().saturating_sub(context_lines);
                if skipped > 0 {
                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }

                for line in &raw[skipped..] {
                    output.push(format!(
                        " {:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                // Skip these context lines entirely.
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    DiffResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

// ---------------------------------------------------------------------------
// Unified patch generation (edit-diff.ts:366-371, jsdiff createTwoFilesPatch)
// ---------------------------------------------------------------------------

/// Generate a standard unified patch.
///
/// Port of `generateUnifiedPatch` (edit-diff.ts:366-371), which delegates to
/// jsdiff's `createTwoFilesPatch` with `FILE_HEADERS_ONLY` (no timestamps, no
/// `Index:` line). The hunk format matches jsdiff output:
/// `@@ -oldStart,oldLines +newStart,newLines @@`.
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let old_tokens = tokenize_lines(old_content);
    let new_tokens = tokenize_lines(new_content);
    let entries = myers_diff(&old_tokens, &new_tokens);

    // Determine which entries to include (changes + nearby context).
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

    // jsdiff always appends a trailing newline.
    output.join("\n") + "\n"
}
