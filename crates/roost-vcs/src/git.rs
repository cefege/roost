//! Git subprocess boundary for linked-worktree metadata, indexes, and checkout hooks.
//! The checkout owner calls these operations; Git remains authoritative for refs and index format.
//! Hooks return captured output so the caller controls forwarding and exit propagation.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::{Error, WorktreeAddOptions};

pub(super) fn clone_preflight(root: &Path) -> Result<(), Error> {
    for key in ["core.sparseCheckout", "core.splitIndex"] {
        let output = Command::new("git")
            .current_dir(root)
            .args(["config", "--bool", "--get", key])
            .output()?;
        if output.status.success() && output.stdout.trim_ascii() == b"true" {
            return Err(Error::Git(format!("{key} is enabled")));
        }
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(Error::Git(stderr_text(&output.stderr)));
        }
    }
    Ok(())
}

pub(super) fn add_worktree_command(
    root: &Path,
    path: &Path,
    reference: &str,
    options: WorktreeAddOptions,
    no_checkout: bool,
) -> Result<(), Error> {
    let hooks_dir = tempfile::Builder::new()
        .prefix("roost-empty-hooks-")
        .tempdir()?;
    let mut hooks_setting = OsString::from("core.hooksPath=");
    hooks_setting.push(hooks_dir.path());
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-c")
        .arg(hooks_setting)
        .args(["worktree", "add", "--quiet"]);
    if no_checkout {
        command.arg("--no-checkout");
    }
    if options.detach {
        command.arg("--detach");
    }
    let output = command
        .arg("--")
        .arg(path)
        .arg(reference)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Git(stderr_text(&output.stderr)))
    }
}

pub(super) fn copy_index(root: &Path, target: &Path) -> Result<(), Error> {
    let source = git_text(
        root,
        [
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-path"),
            OsStr::new("index"),
        ],
    )?;
    let destination = git_text(
        target,
        [
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-path"),
            OsStr::new("index"),
        ],
    )?;
    fs::copy(source, destination)?;
    Ok(())
}

pub(super) fn remove_worktree(root: &Path, path: &Path) -> Result<(), Error> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Git(stderr_text(&output.stderr)))
    }
}

/// Runs the checkout hook once with Git's new-worktree arguments, without forwarding its output.
pub fn run_post_checkout_hook(path: &Path, head: &str) -> Result<Output, Error> {
    let null_oid = "0".repeat(head.len());
    Ok(Command::new("git")
        .current_dir(path)
        .args([
            "hook",
            "run",
            "--ignore-missing",
            "post-checkout",
            "--",
            &null_oid,
            head,
            "1",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()?)
}

pub(super) fn git_text<const N: usize>(cwd: &Path, args: [&OsStr; N]) -> Result<String, Error> {
    let bytes = git_bytes(cwd, args)?;
    String::from_utf8(bytes)
        .map(|text| text.trim().to_owned())
        .map_err(|error| Error::Git(error.to_string()))
}

pub(super) fn git_bytes<const N: usize>(cwd: &Path, args: [&OsStr; N]) -> Result<Vec<u8>, Error> {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::Git(stderr_text(&output.stderr)))
    }
}

fn stderr_text(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).trim().to_owned()
}
