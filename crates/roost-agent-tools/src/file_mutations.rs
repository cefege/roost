//! File-writing and hashline mutation tools, including durable restore points.
//! `ToolHost` supplies the conversation checkpoint; direct file tests omit it.
//! Shared path normalization and restore context live with `file_tools`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use roost_protocol::wire::agent_chat::{EditArgs, WriteArgs};
use tokio::sync::mpsc;

use crate::file_tools::{RestorePointContext, normalize_for_output, resolve_path};
use crate::file_tools::{snapshot_mismatch, unseen_lines_message};
use crate::hashline::{self, EditStore, Operation, PatchSection};
use crate::lsp::LspManager;
use crate::outcome::ToolOutcome;
use crate::restore_points;
/// Write a complete file, creating its parent directory and returning diagnostics.
pub async fn write_tool(
    store: &mut EditStore,
    cwd: &Path,
    args: WriteArgs,
    lsp: &LspManager,
    out: &mpsc::Sender<String>,
    restore: Option<RestorePointContext<'_>>,
) -> ToolOutcome {
    let path = resolve_path(cwd, &args.path);
    if let Some(restore) = restore
        && let Err(error) = restore_points::record_before_change(
            restore.data_dir,
            restore.conversation_id,
            restore.checkpoint_id,
            &path,
        )
    {
        return ToolOutcome::failure(format!("Unable to checkpoint {}: {error}", args.path));
    }
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
    edit_tool_with_restore(store, cwd, args, lsp, out, None).await
}

pub async fn edit_tool_with_restore(
    store: &mut EditStore,
    cwd: &Path,
    args: EditArgs,
    lsp: &LspManager,
    out: &mpsc::Sender<String>,
    restore: Option<RestorePointContext<'_>>,
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
        if let Some(restore) = restore.as_ref() {
            for path in [&edit.path, &result_path] {
                if let Err(error) = restore_points::record_before_change(
                    restore.data_dir,
                    restore.conversation_id,
                    restore.checkpoint_id,
                    path,
                ) {
                    return ToolOutcome::failure(format!(
                        "Unable to checkpoint {}: {error}",
                        edit.display
                    ));
                }
            }
        }
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
