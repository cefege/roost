//! Locating the repository root and walking hand-written source trees.
//! Shared by the size cap, the stdout rule, and the design ratchet so all
//! three agree on which directories are skipped and on repo-relative paths.

use std::path::{Path, PathBuf};

/// Directories that never hold hand-written product source. `target` matters
/// twice over: it is the Cargo build root and the Playwright output root.
const SKIP_DIRS: [&str; 9] = [
    "node_modules",
    ".git",
    "dist",
    ".turbo",
    "target",
    "coverage",
    "test-results",
    "playwright-report",
    ".claude",
];

/// The workspace root, one level above this crate's manifest directory.
/// Built by concatenation rather than `Path::parent` so there is no
/// fallible step here to justify an `expect` in a lint that denies them.
pub fn repo_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

/// Every file under `root`, recursively, in a stable sorted order.
pub fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect(root, &mut found);
    found
}

fn collect(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut sorted: Vec<_> = entries.flatten().collect();
    sorted.sort_by_key(std::fs::DirEntry::path);
    for entry in sorted {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                collect(&entry.path(), found);
            }
        } else if kind.is_file() {
            found.push(entry.path());
        }
    }
}

/// Repo-relative, forward-slashed form of `path`, so baselines and reports are
/// identical on macOS and Linux.
pub fn repo_relative(path: &Path) -> String {
    let root = repo_root();
    let relative = path.strip_prefix(&root).unwrap_or(path);
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Read a UTF-8 source file, or `None` when it is unreadable or binary.
pub fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
