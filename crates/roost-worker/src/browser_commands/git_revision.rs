//! Reads a file from an immutable Git object without touching the worktree or index.
//!
//! Called by `LocalFiles` for revision-qualified browser reads. Git receives arguments
//! directly; commit and blob identifiers are resolved before the bounded object stream.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{Refusal, file_commands::READ_FILE_MAX_BYTES};

const READ_FILE: &str = "read-file";

pub(super) async fn read_git_file(path: &str, revision: &str) -> Result<Vec<u8>, Refusal> {
    let absolute = absolute_path(path)?;
    let (root, relative) = repository_path(&absolute).await?;
    if relative.as_os_str().is_empty() {
        return Err(Refusal::failed(READ_FILE, "a file path is required"));
    }
    let path_arg = if cfg!(windows) {
        relative.to_string_lossy().replace('\\', "/")
    } else {
        relative.to_string_lossy().into_owned()
    };
    let oid = if revision == ":0" {
        index_blob(&root, &path_arg).await?
    } else {
        commit_blob(&root, revision, &path_arg).await?
    };
    let size = git_text(&root, &["cat-file", "-s", &oid]).await?;
    let size = size
        .trim()
        .parse::<u64>()
        .map_err(|_| Refusal::failed(READ_FILE, "Git returned an invalid blob size"))?;
    if size > READ_FILE_MAX_BYTES {
        return Err(Refusal::failed(
            READ_FILE,
            format!("file too large ({size} bytes, max {READ_FILE_MAX_BYTES})"),
        ));
    }
    let mut child = git_command(&root)
        .args(["cat-file", "blob", &oid])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| Refusal::failed(READ_FILE, format!("cannot read Git object: {error}")))?;
    let mut output = Vec::with_capacity(size as usize);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Refusal::failed(READ_FILE, "Git object stream is unavailable"))?;
    stdout
        .take(READ_FILE_MAX_BYTES + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|error| Refusal::failed(READ_FILE, format!("cannot read Git object: {error}")))?;
    if output.len() as u64 > READ_FILE_MAX_BYTES {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(Refusal::failed(
            READ_FILE,
            format!("file too large (max {READ_FILE_MAX_BYTES} bytes)"),
        ));
    }
    let status = child
        .wait()
        .await
        .map_err(|error| Refusal::failed(READ_FILE, format!("cannot read Git object: {error}")))?;
    if !status.success() {
        return Err(Refusal::failed(READ_FILE, "Git could not read the object"));
    }
    Ok(output)
}

fn absolute_path(path: &str) -> Result<PathBuf, Refusal> {
    let path = Path::new(path);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| {
                Refusal::failed(READ_FILE, format!("cannot resolve file path: {error}"))
            })
    }
}

async fn repository_path(path: &Path) -> Result<(PathBuf, PathBuf), Refusal> {
    let mut ancestor = path
        .parent()
        .ok_or_else(|| Refusal::failed(READ_FILE, "file path has no parent"))?;
    loop {
        if tokio::fs::metadata(ancestor).await.is_ok() {
            let canonical = tokio::fs::canonicalize(ancestor).await.map_err(|error| {
                Refusal::failed(READ_FILE, format!("cannot resolve file ancestor: {error}"))
            })?;
            let suffix = path
                .strip_prefix(ancestor)
                .map_err(|_| Refusal::failed(READ_FILE, "file is outside its Git repository"))?;
            let resolved = canonical.join(suffix);
            let output = git_command(&canonical)
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .await;
            if let Ok(output) = output
                && output.status.success()
            {
                let root = git_root_path(&output.stdout)?;
                let root = normalize_windows_path(root);
                let resolved = normalize_windows_path(resolved);
                let relative = resolved.strip_prefix(&root).map_err(|_| {
                    Refusal::failed(READ_FILE, "file is outside its Git repository")
                })?;
                if relative
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
                {
                    return Err(Refusal::failed(
                        READ_FILE,
                        "file is outside its Git repository",
                    ));
                }
                return Ok((root, relative.to_path_buf()));
            }
        }
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Refusal::failed(READ_FILE, "file is not inside a Git repository"))?;
    }
}

fn git_root_path(stdout: &[u8]) -> Result<PathBuf, Refusal> {
    let path = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    let path = if cfg!(windows) {
        path.strip_suffix(b"\r").unwrap_or(path)
    } else {
        path
    };
    std::str::from_utf8(path)
        .map(PathBuf::from)
        .map_err(|_| Refusal::failed(READ_FILE, "Git repository path is not UTF-8"))
}

fn normalize_windows_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{unc}"))
        } else if let Some(disk) = text.strip_prefix(r"\\?\") {
            PathBuf::from(disk)
        } else {
            path
        }
    }
    #[cfg(not(windows))]
    {
        path
    }
}

async fn commit_blob(root: &Path, revision: &str, path: &str) -> Result<String, Refusal> {
    if revision.contains('\n') || revision.contains('\r') || revision.is_empty() {
        return Err(Refusal::failed(READ_FILE, "invalid Git revision"));
    }
    let commit_spec = format!("{revision}^{{commit}}");
    let commit = git_text(
        root,
        &["rev-parse", "--verify", "--end-of-options", &commit_spec],
    )
    .await?;
    let commit = validate_oid(commit.trim())?;
    let output = git_command(root)
        .args([
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            "--full-tree",
            &commit,
            "--",
            path,
        ])
        .output()
        .await
        .map_err(|error| Refusal::failed(READ_FILE, format!("cannot inspect Git tree: {error}")))?;
    if !output.status.success() {
        return Err(Refusal::failed(
            READ_FILE,
            "Git revision does not contain this file",
        ));
    }
    parse_tree_oid(&output.stdout)
}

async fn index_blob(root: &Path, path: &str) -> Result<String, Refusal> {
    let spec = format!(":0:{path}");
    let oid = git_text(root, &["rev-parse", "--verify", "--end-of-options", &spec]).await?;
    validate_oid(oid.trim())
}

fn parse_tree_oid(output: &[u8]) -> Result<String, Refusal> {
    let record = output
        .split(|byte| *byte == 0)
        .next()
        .filter(|record| !record.is_empty())
        .ok_or_else(|| Refusal::failed(READ_FILE, "Git revision does not contain this file"))?;
    let header = record
        .split(|byte| *byte == b'\t')
        .next()
        .ok_or_else(|| Refusal::failed(READ_FILE, "Git tree entry is invalid"))?;
    let mut fields = header
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let mode = fields.next();
    let kind = fields.next();
    let oid = fields.next();
    if mode.is_none() || kind != Some(b"blob") || oid.is_none() || fields.next().is_some() {
        return Err(Refusal::failed(READ_FILE, "Git path is not a file"));
    }
    let oid = std::str::from_utf8(oid.unwrap_or_default())
        .map_err(|_| Refusal::failed(READ_FILE, "Git returned an invalid object id"))?;
    validate_oid(oid)
}

fn validate_oid(oid: &str) -> Result<String, Refusal> {
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Refusal::failed(
            READ_FILE,
            "Git returned an invalid object id",
        ));
    }
    Ok(oid.to_owned())
}
async fn git_text(root: &Path, args: &[&str]) -> Result<String, Refusal> {
    let output = git_command(root)
        .args(args)
        .output()
        .await
        .map_err(|error| Refusal::failed(READ_FILE, format!("cannot run Git: {error}")))?;
    if !output.status.success() {
        return Err(Refusal::failed(
            READ_FILE,
            "Git revision or file is unavailable",
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| Refusal::failed(READ_FILE, "Git returned invalid text"))
}

fn git_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}
#[cfg(test)]
#[path = "git_revision/tests.rs"]
mod tests;
