//! Read files and track the snapshots hashline edits are allowed to change.
//! `file_mutations` owns writes, edits, move/remove and their restore points.
//! The tool host dispatches both read and mutation calls for a conversation.

use std::path::{Path, PathBuf};

use roost_protocol::wire::agent_chat::ReadArgs;

use crate::hashline::{self, EditStore};
use crate::outcome::ToolOutcome;

pub(crate) mod helpers;
pub use crate::file_mutations::{edit_tool, edit_tool_with_restore, write_tool};
pub(crate) use helpers::{snapshot_mismatch, unseen_lines_message};

#[derive(Debug, Clone, Copy)]
pub struct RestorePointContext<'a> {
    pub data_dir: &'a Path,
    pub conversation_id: &'a str,
    pub checkpoint_id: u64,
}

pub(crate) fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

pub(crate) fn normalize_for_output(text: &str) -> String {
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
