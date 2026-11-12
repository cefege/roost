//! Executable lookup for worker-hosted OMP and its inherited subprocess PATH.
//! The worker launcher uses project-local, inherited and user package-manager bins.
//! This host utility has no dependency on an agent engine or tool implementation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Project-local bins, then inherited PATH, then user/package-manager bins.
fn tool_search_dirs(cwd: &Path) -> Vec<PathBuf> {
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
    let mut unique: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// `tool_search_dirs` joined as a `PATH` value for a child process.
pub fn extended_path_value(cwd: &Path) -> OsString {
    std::env::join_paths(tool_search_dirs(cwd))
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

/// The first executable named `name` in the search directories.
pub fn resolve_binary(name: &str, cwd: &Path) -> Option<PathBuf> {
    let candidates: Vec<String> = if cfg!(windows) {
        ["", ".exe", ".cmd", ".bat"]
            .iter()
            .map(|suffix| format!("{name}{suffix}"))
            .collect()
    } else {
        vec![name.to_owned()]
    };
    tool_search_dirs(cwd).into_iter().find_map(|dir| {
        candidates
            .iter()
            .map(|candidate| dir.join(candidate))
            .find(|path| path.is_file())
    })
}

fn home_dir() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn venv_bin_dir(cwd: &Path) -> PathBuf {
    let bin = if cfg!(windows) { "Scripts" } else { "bin" };
    cwd.join(".venv").join(bin)
}
