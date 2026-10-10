//! Ported from oh-my-pi packages/coding-agent/src/lsp/utils.ts (MIT).
//! This file owns conversion between local paths and file URIs.
//! Requests and WorkspaceEdits use these helpers to preserve escaped paths.

use std::path::{Path, PathBuf};

pub(super) fn file_uri(path: &Path) -> Result<String, String> {
    let absolute = path
        .canonicalize()
        .or_else(|_| {
            if path.is_absolute() {
                Ok(path.to_path_buf())
            } else {
                std::env::current_dir().map(|cwd| cwd.join(path))
            }
        })
        .map_err(|error| error.to_string())?;
    url::Url::from_file_path(absolute)
        .map(|uri| uri.to_string())
        .map_err(|_| "path cannot be represented as a file URI".into())
}

pub(super) fn uri_path(uri: &str) -> Result<PathBuf, String> {
    url::Url::parse(uri)
        .map_err(|error| error.to_string())?
        .to_file_path()
        .map_err(|_| format!("not a local file URI: {uri}"))
}
