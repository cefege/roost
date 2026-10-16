//! Unit tests for the pure diff model: numbering, caps, word spans and the
//! output that falls outside the diff.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const ONE_HUNK: &str = "--- a.rs\n+++ a.rs\n@@ -17,4 +17,4 @@\n ctx one\n-let x = 1;\n+let x = 2;\n ctx two\n ctx three\n";

#[test]
fn body_rows_number_each_side_from_the_hunk_header() {
    let rows = parse_unified(ONE_HUNK);
    let numbered: Vec<(RowKind, Option<u32>)> =
        rows.iter().map(|row| (row.kind, row.line_no)).collect();
    assert_eq!(
        numbered,
        vec![
            (RowKind::File, None),
            (RowKind::File, None),
            (RowKind::Hunk, None),
            (RowKind::Context, Some(17)),
            (RowKind::Removed, Some(18)),
            (RowKind::Added, Some(18)),
            (RowKind::Context, Some(19)),
            (RowKind::Context, Some(20)),
        ]
    );
    assert_eq!(rows[4].text, "let x = 1;");
}

#[test]
fn a_removed_line_that_starts_with_dashes_is_body_not_a_file_header() {
    let diff = "@@ -1,2 +1,1 @@\n--- old comment\n-keep\n+kept\n";
    let rows = parse_unified(diff);
    assert_eq!(rows[1].kind, RowKind::Removed);
    assert_eq!(rows[1].text, "-- old comment");
    assert_eq!(rows.len(), 4);
}

#[test]
fn gutter_is_at_least_three_digits_wide() {
    let rows = parse_unified(ONE_HUNK);
    assert_eq!(gutter_width(&rows), 3);
    let wide = parse_unified("@@ -1234,1 +1234,1 @@\n-a\n+b\n");
    assert_eq!(gutter_width(&wide), 4);
}

#[test]
fn single_replacement_highlights_the_changed_token_not_the_indentation() {
    let diff = "@@ -1,1 +1,1 @@\n-    let x = 1;\n+    let x = 2;\n";
    let rows = parse_unified(diff);
    let removed = &rows[1].runs;
    assert!(!removed[0].changed);
    assert!(removed[0].text.starts_with("    "));
    let changed_old: String = removed
        .iter()
        .filter(|run| run.changed)
        .map(|run| run.text.as_str())
        .collect();
    assert!(changed_old.contains('1'));
    assert!(!changed_old.contains("let"));
    let changed_new: String = rows[2]
        .runs
        .iter()
        .filter(|run| run.changed)
        .map(|run| run.text.as_str())
        .collect();
    assert!(changed_new.contains('2'));
}

#[test]
fn word_runs_reassemble_the_original_lines() {
    let (old, new) = word_runs("fn a(x: u8) {", "fn b(x: u16, y: u8) {");
    let joined_old: String = old.iter().map(|run| run.text.as_str()).collect();
    let joined_new: String = new.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(joined_old, "fn a(x: u8) {");
    assert_eq!(joined_new, "fn b(x: u16, y: u8) {");
}

#[test]
fn a_multi_line_replacement_is_not_word_highlighted() {
    let diff = "@@ -1,2 +1,2 @@\n-one\n-two\n+uno\n+dos\n";
    let rows = parse_unified(diff);
    assert!(rows.iter().all(|row| row.runs.is_empty()));
}

#[test]
fn fold_keeps_eight_hunks_and_counts_the_rest() {
    let mut diff = String::new();
    for hunk in 0..10 {
        diff.push_str(&format!(
            "@@ -{0},1 +{0},1 @@\n-old {hunk}\n+new {hunk}\n",
            hunk * 10 + 1
        ));
    }
    let rows = parse_unified(&diff);
    let folded = fold(&rows, COLLAPSED_HUNKS, COLLAPSED_LINES);
    assert_eq!(folded.hidden_hunks, 2);
    assert_eq!(folded.hidden_lines, 20 - 16);
    let hunks = folded
        .rows
        .iter()
        .filter(|row| row.kind == RowKind::Hunk)
        .count();
    assert_eq!(hunks, COLLAPSED_HUNKS);
}

#[test]
fn fold_caps_body_lines_within_a_single_long_hunk() {
    let mut diff = String::from("@@ -1,50 +1,50 @@\n");
    for line in 0..50 {
        diff.push_str(&format!(" context {line}\n"));
    }
    let rows = parse_unified(&diff);
    let folded = fold(&rows, COLLAPSED_HUNKS, COLLAPSED_LINES);
    assert_eq!(folded.hidden_lines, 10);
    assert_eq!(folded.hidden_hunks, 0);
    assert_eq!(folded.rows.len(), 1 + COLLAPSED_LINES);
}

#[test]
fn stats_count_added_and_removed_body_rows_only() {
    let rows = parse_unified(ONE_HUNK);
    assert_eq!(
        diff_stats(&rows),
        DiffStats {
            added: 1,
            removed: 1
        }
    );
}

#[test]
fn output_outside_the_diff_is_kept_for_display() {
    let tag = "4AB2";
    let output = format!(
        "[a.rs#{tag}]\n--- a.rs\n+++ a.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n\nerror[E0308]: mismatched types"
    );
    let diff = "--- a.rs\n+++ a.rs\n@@ -1,1 +1,1 @@\n-a\n+b";
    assert_eq!(
        unexplained_output(&output, diff),
        format!("[a.rs#{tag}]\nerror[E0308]: mismatched types")
    );
}
