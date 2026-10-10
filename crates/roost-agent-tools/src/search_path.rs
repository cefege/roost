//! The directories a tool binary is searched in, in order. A worker runs as a
//! service whose `PATH` lacks the per-user and package-manager directories
//! (`roost-cli` `services/service_environment.rs`), so both the bash tool's
//! `PATH` and the language-server resolver extend it with this one list.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The data directory's language-server bin directory, where the on-demand
/// installer links downloaded servers.
pub fn installed_bin_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-tools").join("lsp").join("bin")
}

/// The search directories for `cwd`: project-local bins, then the inherited
/// `PATH`, then per-user and package-manager directories, then the installer's.
pub fn tool_search_dirs(cwd: &Path, data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.join("node_modules").join(".bin"), venv_bin_dir(cwd)];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(home) = home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
        dirs.push(home.join(".local").join("bin"));
    }
    if !cfg!(windows) {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs.push(installed_bin_dir(data_dir));
    let mut unique: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// `tool_search_dirs` joined as a `PATH` value for a child process.
pub fn extended_path_value(cwd: &Path, data_dir: &Path) -> OsString {
    std::env::join_paths(tool_search_dirs(cwd, data_dir))
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

/// The first executable named `name` in the search directories.
pub fn resolve_binary(name: &str, cwd: &Path, data_dir: &Path) -> Option<PathBuf> {
    let candidates: Vec<String> = if cfg!(windows) {
        ["", ".exe", ".cmd", ".bat"]
            .iter()
            .map(|suffix| format!("{name}{suffix}"))
            .collect()
    } else {
        vec![name.to_owned()]
    };
    tool_search_dirs(cwd, data_dir).into_iter().find_map(|dir| {
        candidates
            .iter()
            .map(|candidate| dir.join(candidate))
            .find(|path| path.is_file())
    })
}

pub fn home_dir() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn venv_bin_dir(cwd: &Path) -> PathBuf {
    let bin = if cfg!(windows) { "Scripts" } else { "bin" };
    cwd.join(".venv").join(bin)
}
