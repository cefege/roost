//! Ported from oh-my-pi crates/pi-edit/src/modes/hashline/messages.rs (MIT).
//! OMP hashline diagnostics rendered by the worker file tools.
//! The edit path uses these messages when a patch references hidden source lines.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

pub(super) fn snapshot_mismatch(
    path: &str,
    expected: &str,
    actual: &str,
    recognized: bool,
    file_text: &str,
    anchor_lines: &[u32],
    tag_origin_paths: &[PathBuf],
) -> String {
    let mut lines = if recognized {
        vec![
            format!("Edit rejected for {path}: file changed between read and edit."),
            format!(
                "Section is bound to #{expected}, but the current file hashes to #{actual}. If a prior edit in this session modified this file, copy the [path#newhash] header from that edit's response; otherwise re-read the file with `read` to refresh the tag before retrying."
            ),
        ]
    } else {
        let mut lines = vec![format!(
            "Edit rejected for {path}: hash #{expected} is not from this session."
        )];
        for origin in tag_origin_paths {
            lines.push(format!(
                "Hash #{expected} was issued in this session for {}.",
                origin.to_string_lossy()
            ));
        }
        lines.push(format!(
            "The current file hashes to #{actual}. Re-read the file with `read` to copy a current [path#tag] header — never invent the tag and never reuse one from a prior session."
        ));
        lines
    };
    let context = anchored_context(anchor_lines, file_text);
    if !context.is_empty() {
        lines.push(String::new());
        lines.extend(context);
    }
    lines.join("\n")
}

fn anchored_context(anchor_lines: &[u32], file_text: &str) -> Vec<String> {
    let file_lines = file_text.split('\n').collect::<Vec<_>>();
    let line_count = u32::try_from(file_lines.len()).unwrap_or(u32::MAX);
    let mut display_lines = BTreeSet::new();
    for line in anchor_lines.iter().copied() {
        if line == 0 || line > line_count {
            continue;
        }
        let start = line.saturating_sub(2).max(1);
        let end = line.saturating_add(2).min(line_count);
        display_lines.extend(start..=end);
    }
    let anchor_set = anchor_lines.iter().copied().collect::<HashSet<_>>();
    let mut rows = Vec::new();
    let mut previous = None;
    for line in display_lines {
        if previous.is_some_and(|last: u32| line > last.saturating_add(1)) {
            rows.push("...".to_owned());
        }
        previous = Some(line);
        let marker = if anchor_set.contains(&line) { "*" } else { " " };
        let text = file_lines
            .get(usize::try_from(line - 1).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or_default();
        rows.push(format!("{marker}{line}:{text}"));
    }
    rows
}

pub(super) fn unseen_lines_message(path: &str, tag: &str, lines: &[u32]) -> String {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let ranges = format_line_ranges(&sorted);
    let selector = ranges.replace(", ", ",");
    format!(
        "This edit anchors to lines {ranges} of {path} that [{path}#{tag}] never displayed (it showed a partial range, a search hit, or a folded summary). Re-read them in full first with a ranged read like `{path}:{selector}` — it skips summarization and mints a fresh tag (a plain re-read just re-folds them) — then re-issue the edit."
    )
}

fn format_line_ranges(lines: &[u32]) -> String {
    let Some(mut start) = lines.first().copied() else {
        return String::new();
    };
    let mut previous = start;
    let mut parts = Vec::new();
    for line in lines.iter().copied().skip(1) {
        if line == previous.saturating_add(1) {
            previous = line;
            continue;
        }
        parts.push(if start == previous {
            start.to_string()
        } else {
            format!("{start}-{previous}")
        });
        start = line;
        previous = line;
    }
    parts.push(if start == previous {
        start.to_string()
    } else {
        format!("{start}-{previous}")
    });
    parts.join(", ")
}
