//! Ported from oh-my-pi packages/coding-agent/src/lsp/edits.ts (MIT).
//! This file owns the local application of LSP WorkspaceEdit text changes.
//! Text positions use UTF-16 code units as required by the LSP wire protocol.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use tokio::fs;

use super::uri::uri_path;

pub(super) async fn apply_workspace_edit(
    cwd: &Path,
    workspace_edit: &Value,
) -> Result<Vec<String>, String> {
    let mut edits_by_uri: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Some(changes) = workspace_edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            if let Some(edits) = edits.as_array() {
                edits_by_uri
                    .entry(uri.clone())
                    .or_default()
                    .extend(edits.clone());
            }
        }
    }
    if let Some(changes) = workspace_edit
        .get("documentChanges")
        .and_then(Value::as_array)
    {
        for change in changes {
            if let (Some(uri), Some(edits)) = (
                change.pointer("/textDocument/uri").and_then(Value::as_str),
                change.get("edits").and_then(Value::as_array),
            ) {
                edits_by_uri
                    .entry(uri.to_owned())
                    .or_default()
                    .extend(edits.clone());
            }
        }
    }
    let mut changed = Vec::new();
    for (uri, edits) in edits_by_uri {
        let path = uri_path(&uri)?;
        let original = fs::read_to_string(&path)
            .await
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let updated = apply_text_edits(&original, &edits)?;
        fs::write(&path, updated)
            .await
            .map_err(|error| format!("{}: {error}", path.display()))?;
        changed.push(relative_path(cwd, &path));
    }
    if let Some(changes) = workspace_edit
        .get("documentChanges")
        .and_then(Value::as_array)
    {
        for change in changes {
            let Some(kind) = change.get("kind").and_then(Value::as_str) else {
                continue;
            };
            if kind == "rename" {
                let old_uri = change
                    .get("oldUri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "rename edit has no oldUri".to_owned())?;
                let target_uri = change
                    .get("newUri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "rename edit has no newUri".to_owned())?;
                let source = uri_path(old_uri)?;
                let target = uri_path(target_uri)?;
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                fs::rename(&source, &target)
                    .await
                    .map_err(|error| error.to_string())?;
                changed.push(format!(
                    "renamed {} to {}",
                    relative_path(cwd, &source),
                    relative_path(cwd, &target)
                ));
                continue;
            }
            let Some(uri) = change.get("uri").and_then(Value::as_str) else {
                continue;
            };
            let path = uri_path(uri)?;
            match kind {
                "create" => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    if !path.exists() {
                        fs::write(&path, [])
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    changed.push(format!("created {}", relative_path(cwd, &path)));
                }
                "delete" => {
                    if path.is_dir() {
                        fs::remove_dir_all(&path)
                            .await
                            .map_err(|error| error.to_string())?;
                    } else if path.exists() {
                        fs::remove_file(&path)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    changed.push(format!("deleted {}", relative_path(cwd, &path)));
                }
                _ => (),
            }
        }
    }
    Ok(changed)
}

pub(super) fn workspace_edit_preview(
    cwd: &Path,
    workspace_edit: &Value,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut add_edits = |uri: &str, edits: &[Value]| -> Result<(), String> {
        let path = uri_path(uri)?;
        lines.push(format!(
            "{}: {} edit(s)",
            relative_path(cwd, &path),
            edits.len()
        ));
        Ok(())
    };
    if let Some(changes) = workspace_edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            if let Some(edits) = edits.as_array() {
                add_edits(uri, edits)?;
            }
        }
    }
    if let Some(changes) = workspace_edit
        .get("documentChanges")
        .and_then(Value::as_array)
    {
        for change in changes {
            if let (Some(uri), Some(edits)) = (
                change.pointer("/textDocument/uri").and_then(Value::as_str),
                change.get("edits").and_then(Value::as_array),
            ) {
                add_edits(uri, edits)?;
            }
        }
    }
    Ok(lines)
}

fn apply_text_edits(original: &str, edits: &[Value]) -> Result<String, String> {
    let mut resolved = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = edit
            .pointer("/range/start")
            .ok_or_else(|| "LSP edit missing range start".to_owned())?;
        let end = edit
            .pointer("/range/end")
            .ok_or_else(|| "LSP edit missing range end".to_owned())?;
        let start = text_offset(original, start)?;
        let end = text_offset(original, end)?;
        let text = edit
            .get("newText")
            .and_then(Value::as_str)
            .ok_or_else(|| "LSP edit missing newText".to_owned())?;
        if start > end {
            return Err("LSP edit range is reversed".into());
        }
        resolved.push((start, end, text.to_owned()));
    }
    resolved.sort_by_key(|edit| std::cmp::Reverse(edit.0));
    for pair in resolved.windows(2) {
        if pair[1].1 > pair[0].0 {
            return Err("overlapping LSP edits".into());
        }
    }
    let mut updated = original.to_owned();
    for (start, end, text) in resolved {
        updated.replace_range(start..end, &text);
    }
    Ok(updated)
}

fn text_offset(text: &str, position: &Value) -> Result<usize, String> {
    let line = position
        .get("line")
        .and_then(Value::as_u64)
        .ok_or_else(|| "LSP position missing line".to_owned())? as usize;
    let character = position
        .get("character")
        .and_then(Value::as_u64)
        .ok_or_else(|| "LSP position missing character".to_owned())? as usize;
    let mut line_start = 0;
    for _ in 0..line {
        let rest = text
            .get(line_start..)
            .ok_or_else(|| "LSP line is outside file".to_owned())?;
        let offset = rest
            .find('\n')
            .ok_or_else(|| "LSP line is outside file".to_owned())?;
        line_start += offset + 1;
    }
    let rest = text
        .get(line_start..)
        .ok_or_else(|| "LSP position is outside file".to_owned())?;
    let line_text = rest
        .split_once('\n')
        .map_or(rest, |(line, _)| line)
        .trim_end_matches('\r');
    let mut utf16 = 0;
    for (byte_offset, character_value) in line_text.char_indices() {
        if utf16 == character {
            return Ok(line_start + byte_offset);
        }
        utf16 += character_value.len_utf16();
    }
    if utf16 == character {
        Ok(line_start + line_text.len())
    } else {
        Err("LSP character is outside line".into())
    }
}

fn relative_path(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::apply_workspace_edit;

    #[tokio::test]
    async fn rename_apply_writes_the_server_workspace_edits()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source.rs");
        tokio::fs::write(&source, "cat\n").await?;
        let uri = url::Url::from_file_path(&source)
            .map_err(|_| "cannot make file URI")?
            .to_string();
        let workspace_edit = json!({"changes":{uri:[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"dog"}]}});
        let changed = apply_workspace_edit(temp.path(), &workspace_edit).await?;
        assert_eq!(changed, vec!["source.rs"]);
        assert_eq!(tokio::fs::read_to_string(source).await?, "dog\n");
        Ok(())
    }
}
