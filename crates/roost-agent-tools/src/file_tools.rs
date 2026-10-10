//! Ported from oh-my-pi crates/pi-edit/src/session.rs (MIT).
//! File tools bind edits to snapshots recorded by `read` and return fresh tags.
//! LSP diagnostics are appended after successful writes when a manager is available.

use crate::hashline::{self, EditStore, Operation, PatchSection};
use crate::lsp::LspManager;
use crate::outcome::ToolOutcome;
use roost_protocol::wire::agent_chat::{EditArgs, ReadArgs, WriteArgs};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
mod helpers;
use helpers::{snapshot_mismatch, unseen_lines_message};
fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn normalize_for_output(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

const READ_MAX_LINES: usize = 2_000;
const READ_MAX_BYTES: usize = 50 * 1024;
const READ_TRUNCATION_NOTE_RESERVE: usize = 64;
/// Read a file or list a directory, formatting file contents as hashline rows.
pub fn read_tool(store: &mut EditStore, cwd: &Path, args: ReadArgs) -> ToolOutcome {
    let path = resolve_path(cwd, &args.path);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return ToolOutcome::failure(format!("Unable to read {}: {error}", args.path));
        }
    };
    if metadata.is_dir() {
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                return ToolOutcome::failure(format!("Unable to list {}: {error}", args.path));
            }
        };
        let mut names = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        return ToolOutcome::success(names.join("\n"));
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return ToolOutcome::failure(format!("Unable to read {}: {error}", args.path));
        }
    };
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            return ToolOutcome::failure(format!("Unable to read {} as UTF-8: {error}", args.path));
        }
    };
    let normalized = normalize_for_output(&text);
    let tag = store.record_snapshot(&path, &normalized);
    let mut all_lines = normalized.split('\n').collect::<Vec<_>>();
    if normalized.ends_with('\n') {
        all_lines.pop();
    }
    let offset = args.offset.unwrap_or(1).max(1);
    let start = usize::try_from(offset - 1)
        .unwrap_or(usize::MAX)
        .min(all_lines.len());
    let requested = args
        .limit
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
        .unwrap_or(READ_MAX_LINES)
        .min(READ_MAX_LINES);
    let mut output = hashline::format_header(&args.path, &tag);
    let mut seen = Vec::new();
    let mut emitted = 0usize;
    for (idx, line) in all_lines.iter().enumerate().skip(start).take(requested) {
        let row = hashline::line_prefix(u32::try_from(idx + 1).unwrap_or(u32::MAX), line);
        if output
            .len()
            .saturating_add(row.len())
            .saturating_add(1)
            .saturating_add(READ_TRUNCATION_NOTE_RESERVE)
            > READ_MAX_BYTES
        {
            break;
        }
        output.push('\n');
        output.push_str(&row);
        emitted += 1;
        seen.push(u32::try_from(idx + 1).unwrap_or(u32::MAX));
    }
    hashline::record_seen_lines(store, &path, &seen);
    let omitted = all_lines.len().saturating_sub(start + emitted);
    if omitted > 0 {
        output.push_str(&format!("\n… {omitted} more lines; use offset"));
    }
    ToolOutcome::success(output)
}

/// Write a complete file, creating its parent directory and returning diagnostics.
pub async fn write_tool(
    store: &mut EditStore,
    cwd: &Path,
    args: WriteArgs,
    lsp: &LspManager,
    out: &mpsc::Sender<String>,
) -> ToolOutcome {
    let path = resolve_path(cwd, &args.path);
    if let Some(parent) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        return ToolOutcome::failure(format!("Unable to create parent directory: {error}"));
    }
    if let Err(error) = atomic_write(&path, args.content.as_bytes()).await {
        return ToolOutcome::failure(format!("Unable to write {}: {error}", args.path));
    }
    let tag = store.record_snapshot(&path, &args.content);
    let line_count = args.content.split('\n').count() - usize::from(args.content.ends_with('\n'));
    let seen = (1..=u32::try_from(line_count).unwrap_or(u32::MAX)).collect::<Vec<_>>();
    hashline::record_seen_lines_for_tag(store, &path, &tag, &seen);
    let mut result = hashline::format_header(&args.path, &tag);
    if let Some(diagnostics) = lsp
        .diagnostics_after_write(cwd, &path, &args.content, out)
        .await
    {
        result.push('\n');
        result.push_str(&diagnostics);
    }
    ToolOutcome::success(result)
}

/// Apply a hashline patch after checking its snapshot tag, then return a diff preview.
pub async fn edit_tool(
    store: &mut EditStore,
    cwd: &Path,
    args: EditArgs,
    lsp: &LspManager,
    out: &mpsc::Sender<String>,
) -> ToolOutcome {
    let sections = match hashline::parse_sections(&args.input) {
        Ok(sections) => sections,
        Err(error) => return ToolOutcome::failure(error),
    };
    let sections = match coalesce_sections(cwd, sections) {
        Ok(sections) => sections,
        Err(error) => return ToolOutcome::failure(error),
    };
    let mut transaction = store.clone();
    transaction.clear_anonymous_register();
    let mut staged = Vec::new();
    for section in sections {
        let path = resolve_path(cwd, &section.path);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return ToolOutcome::failure(format!("Unable to read {}: {error}", section.path));
            }
        };
        let before = match String::from_utf8(bytes) {
            Ok(text) => normalize_for_output(&text),
            Err(error) => {
                return ToolOutcome::failure(format!(
                    "Unable to read {} as UTF-8: {error}",
                    section.path
                ));
            }
        };
        let actual_tag = hashline::compute_tag(&before);
        let touched = touched_lines(&section);
        let recognized = store.snapshot(&path, &section.tag).is_some();
        if !recognized || !actual_tag.eq_ignore_ascii_case(&section.tag) {
            let tag_origin_paths = store
                .paths_with_tag(&section.tag)
                .into_iter()
                .filter(|origin| origin != &path)
                .collect::<Vec<_>>();
            return ToolOutcome::failure(snapshot_mismatch(
                &section.path,
                &section.tag,
                &actual_tag,
                recognized,
                &before,
                &touched,
                &tag_origin_paths,
            ));
        }
        if !store.has_seen_lines(&path, &section.tag, &touched) {
            let message = unseen_lines_message(&section.path, &section.tag, &touched);
            return ToolOutcome::failure(message);
        }
        let after = match hashline::apply_operations(&before, &section.operations, &mut transaction)
        {
            Ok(after) => after,
            Err(error) => return ToolOutcome::failure(error),
        };
        let remove = section
            .operations
            .iter()
            .any(|operation| matches!(operation, Operation::Remove));
        let move_to = section
            .operations
            .iter()
            .find_map(|operation| match operation {
                Operation::Move(destination) => Some(destination.clone()),
                _ => None,
            });
        staged.push(StagedEdit {
            display: section.path,
            path,
            before,
            after,
            remove,
            move_to,
        });
    }
    for edit in &staged {
        let result_path = edit
            .move_to
            .as_ref()
            .map(|destination| resolve_path(cwd, destination))
            .unwrap_or_else(|| edit.path.clone());
        if edit.remove {
            if let Err(error) = tokio::fs::remove_file(&edit.path).await {
                return ToolOutcome::failure(format!("Unable to remove {}: {error}", edit.display));
            }
        } else {
            if let Some(parent) = result_path.parent()
                && let Err(error) = tokio::fs::create_dir_all(parent).await
            {
                return ToolOutcome::failure(format!("Unable to create parent directory: {error}"));
            }
            if let Err(error) = atomic_write(&result_path, edit.after.as_bytes()).await {
                return ToolOutcome::failure(format!("Unable to write {}: {error}", edit.display));
            }
            if result_path != edit.path
                && let Err(error) = tokio::fs::remove_file(&edit.path).await
            {
                return ToolOutcome::failure(format!("Unable to move {}: {error}", edit.display));
            }
        }
    }
    let mut results = Vec::new();
    let mut diffs = Vec::new();
    for edit in staged {
        if edit.remove {
            let diff = format!("--- {}\n+++ /dev/null\n", edit.display);
            diffs.push(diff.clone());
            let tag = transaction.record_snapshot(&edit.path, "");
            results.push(format!(
                "{}\n{diff}",
                hashline::format_header(&edit.display, &tag)
            ));
            continue;
        }
        let result_display = edit.move_to.as_deref().unwrap_or(&edit.display);
        let result_path = edit
            .move_to
            .as_ref()
            .map(|destination| resolve_path(cwd, destination))
            .unwrap_or_else(|| edit.path.clone());
        if result_path != edit.path {
            transaction.relocate_path(&edit.path, &result_path);
        }
        let tag = transaction.record_snapshot(&result_path, &edit.after);
        let diff = unified_preview(result_display, &edit.before, &edit.after);
        diffs.push(diff.clone());
        let mut content = format!("{}\n{diff}", hashline::format_header(result_display, &tag));
        if let Some(diagnostics) = lsp
            .diagnostics_after_write(cwd, &result_path, &edit.after, out)
            .await
        {
            content.push('\n');
            content.push_str(&diagnostics);
        }
        results.push(content);
    }
    *store = transaction;
    ToolOutcome::success(results.join("\n\n"))
        .with_details(serde_json::json!({"diff": diffs.join("\n\n")}))
}
#[derive(Debug)]
struct StagedEdit {
    display: String,
    path: PathBuf,
    before: String,
    after: String,
    remove: bool,
    move_to: Option<String>,
}

fn coalesce_sections(cwd: &Path, sections: Vec<PatchSection>) -> Result<Vec<PatchSection>, String> {
    let mut coalesced: Vec<PatchSection> = Vec::new();
    let mut indexes = std::collections::HashMap::new();
    for section in sections {
        let path = resolve_path(cwd, &section.path);
        if let Some(index) = indexes.get(&path).copied() {
            let existing: &mut PatchSection = &mut coalesced[index];
            if existing.tag != section.tag {
                return Err(format!(
                    "Conflicting hashline snapshot tags for {}: #{} and #{}. Re-read the file and retry with one current header.",
                    section.path, existing.tag, section.tag
                ));
            }
            existing.operations.extend(section.operations);
        } else {
            indexes.insert(path, coalesced.len());
            coalesced.push(section);
        }
    }
    Ok(coalesced)
}

fn touched_lines(section: &PatchSection) -> Vec<u32> {
    let mut touched = Vec::new();
    for operation in &section.operations {
        let range = match operation {
            Operation::Replace { start, end, .. }
            | Operation::Cut { start, end, .. }
            | Operation::PasteRange { start, end, .. } => Some((*start, *end)),
            Operation::InsertBefore { line: 1, .. } | Operation::PasteBefore { line: 1, .. } => {
                None
            }
            Operation::InsertBefore { line, .. }
            | Operation::PasteBefore { line, .. }
            | Operation::InsertAfter { line, .. }
            | Operation::PasteAfter { line, .. } => Some((*line, *line)),
            Operation::InsertEnd { .. }
            | Operation::PasteEnd { .. }
            | Operation::Remove
            | Operation::Move(_) => None,
        };
        if let Some((start, end)) = range {
            touched.extend(start..=end);
        }
    }
    touched
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(std::io::Error::other)?;
    let mut suffix = String::with_capacity(entropy.len() * 2);
    for byte in entropy {
        let _ = write!(suffix, "{byte:02x}");
    }
    let temporary = parent.join(format!(".{name}.{suffix}.tmp"));
    tokio::fs::write(&temporary, bytes).await?;
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    Ok(())
}

fn unified_preview(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return "(no changes)".to_owned();
    }
    let mut output = format!("--- {path}\n+++ {path}\n");
    for line in before.split('\n') {
        output.push('-');
        output.push_str(line);
        output.push('\n');
    }
    for line in after.split('\n') {
        output.push('+');
        output.push_str(line);
        output.push('\n');
    }
    output
}
