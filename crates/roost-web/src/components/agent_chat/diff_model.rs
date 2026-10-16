//! The edit box's diff as pure rows: the unified diff the executor reports,
//! numbered per side, folded to OMP's hunk and line caps, and word-highlighted
//! where one removed line is replaced by one added line. Nothing here touches
//! the DOM; `diff_view` renders the rows.

use similar::{ChangeTag, TextDiff};

/// OMP's collapsed edit preview shows at most this many hunks.
pub(super) const COLLAPSED_HUNKS: usize = 8;
/// OMP's collapsed edit preview shows at most this many diff body lines.
pub(super) const COLLAPSED_LINES: usize = 40;

/// What one diff row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowKind {
    /// `--- path` / `+++ path` of one file in a multi-file edit.
    File,
    /// `@@ -a,b +c,d @@`.
    Hunk,
    Context,
    Added,
    Removed,
}

/// A run of one line's text, flagged when word-level diffing changed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WordRun {
    pub(super) text: String,
    pub(super) changed: bool,
}

/// One rendered diff row. `line_no` is the gutter number on the side the row
/// belongs to: the old side for removals, the new side otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiffRow {
    pub(super) kind: RowKind,
    pub(super) line_no: Option<u32>,
    pub(super) text: String,
    /// Empty unless the row is one half of a single-line replacement.
    pub(super) runs: Vec<WordRun>,
}

/// The `[+N/−M]` stats of a diff.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct DiffStats {
    pub(super) added: usize,
    pub(super) removed: usize,
}

/// A diff after the caps, with what the caps hid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FoldedDiff {
    pub(super) rows: Vec<DiffRow>,
    pub(super) hidden_hunks: usize,
    pub(super) hidden_lines: usize,
}

/// The unified diff's row model. Only lines inside a hunk's declared line
/// counts are body rows, so a removed line that starts with `--` is not taken
/// for a file header.
pub(super) fn parse_unified(diff: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_no = 0_u32;
    let mut new_no = 0_u32;
    let mut old_left = 0_u32;
    let mut new_left = 0_u32;
    for line in diff.lines() {
        if old_left == 0 && new_left == 0 {
            if let Some(hunk) = parse_hunk_header(line) {
                old_no = hunk.old_start;
                new_no = hunk.new_start;
                old_left = hunk.old_count;
                new_left = hunk.new_count;
                rows.push(plain_row(RowKind::Hunk, line));
            } else if line.starts_with("--- ") || line.starts_with("+++ ") {
                rows.push(plain_row(RowKind::File, line));
            }
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            rows.push(body_row(RowKind::Added, Some(new_no), text));
            new_no += 1;
            new_left = new_left.saturating_sub(1);
        } else if let Some(text) = line.strip_prefix('-') {
            rows.push(body_row(RowKind::Removed, Some(old_no), text));
            old_no += 1;
            old_left = old_left.saturating_sub(1);
        } else if line.starts_with('\\') {
            continue;
        } else {
            let text = line.strip_prefix(' ').unwrap_or(line);
            rows.push(body_row(RowKind::Context, Some(new_no), text));
            old_no += 1;
            new_no += 1;
            old_left = old_left.saturating_sub(1);
            new_left = new_left.saturating_sub(1);
        }
    }
    highlight_single_replacements(&mut rows);
    rows
}

/// The `[+N/−M]` stats: every body row of each kind, across files.
pub(super) fn diff_stats(rows: &[DiffRow]) -> DiffStats {
    rows.iter()
        .fold(DiffStats::default(), |stats, row| match row.kind {
            RowKind::Added => DiffStats {
                added: stats.added + 1,
                ..stats
            },
            RowKind::Removed => DiffStats {
                removed: stats.removed + 1,
                ..stats
            },
            _ => stats,
        })
}

/// The gutter's digit width: the widest line number, never under three.
pub(super) fn gutter_width(rows: &[DiffRow]) -> usize {
    let widest = rows
        .iter()
        .filter_map(|row| row.line_no)
        .map(|number| number.to_string().len())
        .max()
        .unwrap_or(0);
    widest.max(3)
}

/// Keeps at most `hunk_cap` hunks and `line_cap` body lines, and counts what
/// was hidden. A file header is kept only while its hunks are shown.
pub(super) fn fold(rows: &[DiffRow], hunk_cap: usize, line_cap: usize) -> FoldedDiff {
    let total_hunks = rows.iter().filter(|row| row.kind == RowKind::Hunk).count();
    let total_lines = rows.iter().filter(|row| is_body(row.kind)).count();
    let mut kept = Vec::new();
    let mut hunks_kept = 0;
    let mut lines_kept = 0;
    for row in rows {
        match row.kind {
            RowKind::Hunk => {
                if hunks_kept == hunk_cap {
                    break;
                }
                hunks_kept += 1;
            }
            RowKind::File => {
                if hunks_kept == hunk_cap {
                    break;
                }
            }
            _ => {
                if lines_kept == line_cap {
                    break;
                }
                lines_kept += 1;
            }
        }
        kept.push(row.clone());
    }
    FoldedDiff {
        rows: kept,
        hidden_hunks: total_hunks - hunks_kept,
        hidden_lines: total_lines - lines_kept,
    }
}

/// The output lines the diff does not account for, such as LSP diagnostics
/// appended after an edit. Lines the diff itself produced are dropped.
pub(super) fn unexplained_output(output: &str, diff: &str) -> String {
    let explained: std::collections::BTreeSet<&str> = diff.lines().collect();
    output
        .lines()
        .filter(|line| !explained.contains(line) && !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_body(kind: RowKind) -> bool {
    matches!(kind, RowKind::Context | RowKind::Added | RowKind::Removed)
}

fn plain_row(kind: RowKind, text: &str) -> DiffRow {
    DiffRow {
        kind,
        line_no: None,
        text: text.to_owned(),
        runs: Vec::new(),
    }
}

fn body_row(kind: RowKind, line_no: Option<u32>, text: &str) -> DiffRow {
    DiffRow {
        kind,
        line_no,
        text: text.to_owned(),
        runs: Vec::new(),
    }
}

struct HunkHeader {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

/// `@@ -a[,b] +c[,d] @@`; a missing count is one line, as in unified diffs.
fn parse_hunk_header(line: &str) -> Option<HunkHeader> {
    let body = line.strip_prefix("@@ -")?;
    let (old, rest) = body.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let (old_start, old_count) = range_pair(old)?;
    let (new_start, new_count) = range_pair(new)?;
    Some(HunkHeader {
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

fn range_pair(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

/// Word-highlights each removed line that is the only line replaced by an
/// added line, as OMP does; larger replacements stay whole-line.
fn highlight_single_replacements(rows: &mut [DiffRow]) {
    let mut idx = 0;
    while idx < rows.len() {
        if rows[idx].kind != RowKind::Removed {
            idx += 1;
            continue;
        }
        let removed_end = run_end(rows, idx, RowKind::Removed);
        let added_end = run_end(rows, removed_end, RowKind::Added);
        if removed_end - idx == 1 && added_end - removed_end == 1 {
            let (old_runs, new_runs) = word_runs(&rows[idx].text, &rows[removed_end].text);
            rows[idx].runs = old_runs;
            rows[removed_end].runs = new_runs;
        }
        idx = added_end.max(idx + 1);
    }
}

fn run_end(rows: &[DiffRow], start: usize, kind: RowKind) -> usize {
    rows[start..]
        .iter()
        .position(|row| row.kind != kind)
        .map_or(rows.len(), |offset| start + offset)
}

/// Word-level diff of an old and a new line. Leading indentation is never
/// highlighted, so the change reads from the first word that moved.
pub(super) fn word_runs(old: &str, new: &str) -> (Vec<WordRun>, Vec<WordRun>) {
    let diff = TextDiff::from_words(old, new);
    let mut old_chars = Vec::new();
    let mut new_chars = Vec::new();
    for change in diff.iter_all_changes() {
        let value = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                push_flagged(&mut old_chars, value, false);
                push_flagged(&mut new_chars, value, false);
            }
            ChangeTag::Delete => push_flagged(&mut old_chars, value, true),
            ChangeTag::Insert => push_flagged(&mut new_chars, value, true),
        }
    }
    (group_runs(old_chars), group_runs(new_chars))
}

fn push_flagged(chars: &mut Vec<(char, bool)>, value: &str, changed: bool) {
    chars.extend(value.chars().map(|ch| (ch, changed)));
}

fn group_runs(mut chars: Vec<(char, bool)>) -> Vec<WordRun> {
    for entry in chars.iter_mut().take_while(|(ch, _)| ch.is_whitespace()) {
        entry.1 = false;
    }
    let mut runs: Vec<WordRun> = Vec::new();
    for (ch, changed) in chars {
        match runs.last_mut() {
            Some(run) if run.changed == changed => run.text.push(ch),
            _ => runs.push(WordRun {
                text: ch.to_string(),
                changed,
            }),
        }
    }
    runs
}

#[cfg(test)]
#[path = "diff_model_tests.rs"]
mod tests;
