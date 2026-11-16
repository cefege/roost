//! Git-linked worktree creation: a reflink clone of the source tree reconciled to
//! the target commit, falling back to a plain checkout. Called by roost-cli
//! `worktree add`; Git owns refs, index and checkout.

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
mod clone;
mod git;
mod state;

pub use git::run_post_checkout_hook;

use clone::{clone_source_tree, copy_directory_modes, install_staging};
use git::{add_worktree_command, clone_preflight, copy_index, git_text, remove_worktree};
use state::remove_source_untracked;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeAddOptions {
    pub detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeAddResult {
    pub cloned: bool,
    pub clone_error: Option<String>,
    pub head: String,
    pub subject: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git worktree add: {0}")]
    Git(String),
    #[error("git worktree add: {0}")]
    Io(#[from] std::io::Error),
}

/// Add a linked checkout at `path`, with Git as the authority for refs/index/checkout.
pub fn worktree_add(
    root: &Path,
    path: &Path,
    reference: &str,
    options: WorktreeAddOptions,
) -> Result<WorktreeAddResult, Error> {
    let root = git_text(
        root,
        [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
    )?;
    let root = fs::canonicalize(root)?;
    let path = absolute_from(&root, path);
    ensure_destination(&path)?;
    let commit_ref = format!("{reference}^{{commit}}");
    let commit = git_text(
        &root,
        [
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--end-of-options"),
            OsStr::new(&commit_ref),
        ],
    )?;

    let mut clone_error = None;
    let staging = match clone_preflight(&root) {
        Ok(()) => match clone_source_tree(&root) {
            Ok(path) => Some(path),
            Err(error) => {
                clone_error = Some(error.to_string());
                None
            }
        },
        Err(error) => {
            clone_error = Some(error.to_string());
            None
        }
    };
    let cloned = if let Some(staging) = staging {
        // Only successful registration authorizes cleanup of this destination.
        add_worktree_command(&root, &path, reference, options, true)?;
        match reconcile_clone(&root, &path, &commit, staging.path()) {
            Ok(()) => true,
            Err(error) => {
                clone_error = Some(error.to_string());
                remove_worktree(&root, &path)?;
                add_worktree_command(&root, &path, reference, options, false)?;
                false
            }
        }
    } else {
        add_worktree_command(&root, &path, reference, options, false)?;
        false
    };
    let head = git_text(&path, [OsStr::new("rev-parse"), OsStr::new("HEAD")])?;
    let subject = git_text(
        &path,
        [
            OsStr::new("log"),
            OsStr::new("-1"),
            OsStr::new("--format=%s"),
        ],
    )?;
    if let Some(error) = clone_error.as_deref() {
        tracing::warn!(path = %path.display(), error, "worktree clone fell back to plain checkout");
    }
    tracing::info!(source = %root.display(), path = %path.display(), cloned, "linked worktree created");
    Ok(WorktreeAddResult {
        cloned,
        clone_error,
        head,
        subject,
    })
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

fn ensure_destination(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(Error::Git(format!("'{}' already exists", path.display())))
        }
        Ok(_) if fs::read_dir(path)?.next().is_some() => {
            Err(Error::Git(format!("'{}' already exists", path.display())))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn reconcile_clone(root: &Path, path: &Path, commit: &str, staging: &Path) -> Result<(), Error> {
    install_staging(staging, path)?;
    copy_index(root, path)?;
    remove_source_untracked(root, path)?;
    git_text(
        path,
        [
            OsStr::new("read-tree"),
            OsStr::new("--reset"),
            OsStr::new("-u"),
            OsStr::new(commit),
        ],
    )?;
    copy_directory_modes(root, path)
}

#[cfg(test)]
mod tests;
