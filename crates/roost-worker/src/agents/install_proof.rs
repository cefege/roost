//! Filesystem proofs the agent-integration installer plans and commits
//! against: file and directory identities, ownership markers, and the path key
//! that decides whether two loader directories are one. Ports v2
//! `apps/worker/src/agents/integration-install-proof.ts`; called by
//! [`super::install_integrations`] and the staged install transaction.

use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use roost_platform::HostPlatform;
use unicode_normalization::UnicodeNormalization as _;

/// A regular file as it was read: its text and the inode that held it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationFileSnapshot {
    pub content: String,
    pub device: u64,
    pub inode: u64,
}

/// A loader directory's entry (possibly a symlink) and the directory it
/// resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDirectorySnapshot {
    pub canonical_path: PathBuf,
    pub entry_is_symlink: bool,
    pub entry_device: u64,
    pub entry_inode: u64,
    pub directory_device: u64,
    pub directory_inode: u64,
}

/// A loader directory before any mutation: `initial_snapshot` is `None` when
/// the directory does not exist yet and the install will create it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDirectoryPlan {
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    pub initial_snapshot: Option<IntegrationDirectorySnapshot>,
}

/// What the commit guard re-checks for one target. `remove` is `Some` only for
/// a retirement: `Some(false)` lets a file that is not Roost's stay unowned.
#[derive(Debug, Clone, Copy)]
pub struct IntegrationTargetPlan<'a> {
    pub target: &'a Path,
    pub ownership_marker: &'a str,
    pub existing: Option<&'a IntegrationFileSnapshot>,
    pub remove: Option<bool>,
}

/// A refusal the installer reports; every one is a plain message, as in v2.
pub(crate) fn refusal(message: String) -> io::Error {
    io::Error::other(message)
}

/// The key two paths must share to be the same loader directory. macOS and
/// Windows file systems are case-insensitive by default, Linux is not.
pub fn integration_path_comparison_key(path: &Path, platform: HostPlatform) -> String {
    let normalized: String = path.to_string_lossy().nfc().collect();
    match platform {
        HostPlatform::MacOs | HostPlatform::Windows => normalized.to_lowercase(),
        HostPlatform::Linux => normalized,
    }
}

/// Ownership is a `//` comment line carrying the marker as its own
/// whitespace-delimited token, at ANY depth: an installed asset splices the
/// report transport above the integration's own header, so its marker sits
/// ~100 lines down. Depth is not evidence of authorship; the token is.
pub fn has_integration_ownership(content: &str, marker: &str) -> bool {
    if !content.contains(marker) {
        return false;
    }
    content.split('\n').any(|line| {
        let line = line.strip_suffix('\r').unwrap_or(line);
        line.strip_prefix("//")
            .is_some_and(|comment| comment.split_whitespace().any(|token| token == marker))
    })
}

/// The directory as it stands, or — when absent — the canonical path it will
/// have once created, so a collision is caught before anything is written.
pub fn preflight_integration_directory(path: &Path) -> io::Result<IntegrationDirectoryPlan> {
    match inspect_integration_directory(path) {
        Ok(snapshot) => Ok(IntegrationDirectoryPlan {
            path: path.to_path_buf(),
            canonical_path: snapshot.canonical_path.clone(),
            initial_snapshot: Some(snapshot),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(IntegrationDirectoryPlan {
            path: path.to_path_buf(),
            canonical_path: canonicalize_planned_path(&lexically_normalize(path))?,
            initial_snapshot: None,
        }),
        Err(error) => Err(error),
    }
}

/// Read a target without following a link, proving the inode did not change
/// between the two stats around the read. `None` when nothing is there.
pub fn inspect_integration_target(
    path: &Path,
    description: &str,
) -> io::Result<Option<IntegrationFileSnapshot>> {
    let Some(before) = integration_lstat_if_present(path)? else {
        return Ok(None);
    };
    if before.file_type().is_symlink() {
        return Err(refusal(format!(
            "refusing symlink {description}: {}",
            path.display()
        )));
    }
    if !before.is_file() {
        return Err(refusal(format!(
            "refusing unsafe {description}: {}",
            path.display()
        )));
    }
    // v2 reads with `utf8`, which replaces an invalid sequence rather than
    // refusing the file; the marker test runs on that same decoded text.
    let content = String::from_utf8_lossy(&fs::read(path)?).into_owned();
    let after = fs::symlink_metadata(path)?;
    if !after.is_file() || !same_integration_identity(&before, after.dev(), after.ino()) {
        return Err(refusal(format!(
            "{description} changed while being inspected: {}",
            path.display()
        )));
    }
    Ok(Some(IntegrationFileSnapshot {
        content,
        device: after.dev(),
        inode: after.ino(),
    }))
}

/// The commit guard: the target is still exactly what planning saw, and still
/// Roost's unless the plan already knew it was not.
pub fn assert_integration_target_unchanged(plan: IntegrationTargetPlan<'_>) -> io::Result<()> {
    let changed = || {
        refusal(format!(
            "agent integration target changed before commit: {}",
            plan.target.display()
        ))
    };
    let current = inspect_integration_target(plan.target, "agent integration target")
        .map_err(|_| changed())?;
    if !same_integration_file_snapshot(current.as_ref(), plan.existing) {
        return Err(changed());
    }
    if let Some(current) = current
        && !has_integration_ownership(&current.content, plan.ownership_marker)
    {
        if plan.remove == Some(false) {
            return Ok(());
        }
        return Err(refusal(format!(
            "agent integration target ownership changed before commit: {}",
            plan.target.display()
        )));
    }
    Ok(())
}

/// The loader entry and the directory it resolves to.
pub fn inspect_integration_directory(path: &Path) -> io::Result<IntegrationDirectorySnapshot> {
    let entry = fs::symlink_metadata(path)?;
    let canonical_path = fs::canonicalize(path).map_err(|_| {
        refusal(format!(
            "refusing dangling agent integration loader: {}",
            path.display()
        ))
    })?;
    let directory = fs::symlink_metadata(&canonical_path)?;
    if !directory.is_dir() {
        return Err(refusal(format!(
            "refusing non-directory agent integration loader: {}",
            path.display()
        )));
    }
    Ok(IntegrationDirectorySnapshot {
        canonical_path,
        entry_is_symlink: entry.file_type().is_symlink(),
        entry_device: entry.dev(),
        entry_inode: entry.ino(),
        directory_device: directory.dev(),
        directory_inode: directory.ino(),
    })
}

/// Refuse a loader directory whose entry or target moved since `expected`.
pub fn assert_integration_directory_snapshot(
    plan: &IntegrationDirectoryPlan,
    expected: &IntegrationDirectorySnapshot,
    platform: HostPlatform,
) -> io::Result<()> {
    let changed = || {
        refusal(format!(
            "agent integration loader changed during installation: {}",
            plan.path.display()
        ))
    };
    let current = inspect_integration_directory(&plan.path).map_err(|_| changed())?;
    let same = current.entry_is_symlink == expected.entry_is_symlink
        && current.entry_device == expected.entry_device
        && current.entry_inode == expected.entry_inode
        && current.directory_device == expected.directory_device
        && current.directory_inode == expected.directory_inode
        && integration_path_comparison_key(&current.canonical_path, platform)
            == integration_path_comparison_key(&expected.canonical_path, platform);
    if same { Ok(()) } else { Err(changed()) }
}

/// `lstat`, with an absent path as `None` rather than an error.
pub fn integration_lstat_if_present(path: &Path) -> io::Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn same_integration_file_snapshot(
    left: Option<&IntegrationFileSnapshot>,
    right: Option<&IntegrationFileSnapshot>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.content == right.content
                && left.device == right.device
                && left.inode == right.inode
        }
        (left, right) => left.is_none() && right.is_none(),
    }
}

pub fn same_integration_identity(metadata: &Metadata, device: u64, inode: u64) -> bool {
    metadata.dev() == device && metadata.ino() == inode
}

/// Node's `path.join`/`resolve` collapse `.` and `..` by text; the planned
/// paths must be spelled the same way before they are compared or created.
pub(crate) fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => normalized.push(".."),
            },
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// The real path of the deepest existing ancestor, with the absent tail
/// appended: where an absent directory WILL be once it is created.
fn canonicalize_planned_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        lexically_normalize(&std::env::current_dir()?.join(path))
    };
    match fs::canonicalize(&absolute) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match (absolute.parent(), absolute.file_name()) {
                (Some(parent), Some(name)) => Ok(canonicalize_planned_path(parent)?.join(name)),
                _ => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}
