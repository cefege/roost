//! Ported from oh-my-pi packages/coding-agent/src/lsp/config.ts (MIT).
//! This file owns cwd-local root-marker selection and file-type matching.
//! It never walks to a parent directory when choosing language servers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::types::ServerConfig;

pub(super) fn server_configurations() -> BTreeMap<String, ServerConfig> {
    serde_json::from_str(include_str!("servers.json")).unwrap_or_default()
}

pub(super) fn selected_servers(cwd: &Path, file: &Path) -> Vec<(String, ServerConfig, PathBuf)> {
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|value| format!(".{value}"));
    server_configurations()
        .into_iter()
        .filter_map(|(name, config)| {
            if !config
                .file_types
                .iter()
                .any(|value| Some(value.as_str()) == extension.as_deref())
            {
                return None;
            }
            if !config
                .root_markers
                .iter()
                .any(|marker| marker_exists(cwd, marker))
            {
                return None;
            }
            Some((name, config, cwd.to_path_buf()))
        })
        .collect()
}

pub(super) fn configured_servers(cwd: &Path, data_dir: &Path) -> Vec<String> {
    server_configurations()
        .into_iter()
        .filter_map(|(name, config)| {
            let applies = config
                .root_markers
                .iter()
                .any(|marker| marker_exists(cwd, marker));
            let available =
                crate::search_path::resolve_binary(&config.command, cwd, data_dir).is_some();
            (applies && !available).then_some(format!(
                "{name} (configured, missing binary {})",
                config.command
            ))
        })
        .collect()
}

pub(super) fn language_id(config: &ServerConfig, path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    config
        .language_ids
        .get(&extension)
        .cloned()
        .unwrap_or_else(|| super::render::language_id(path))
}

fn marker_exists(cwd: &Path, marker: &str) -> bool {
    if !marker.contains('*') && !marker.contains('?') {
        return cwd.join(marker).exists();
    }
    let Ok(pattern) = globset::Glob::new(marker) else {
        return false;
    };
    let matcher = pattern.compile_matcher();
    cwd.read_dir()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| matcher.is_match(entry.file_name()))
}
