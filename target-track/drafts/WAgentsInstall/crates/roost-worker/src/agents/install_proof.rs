//! Filesystem proofs the agent-integration installer plans and commits against:
//! file and directory identities, ownership recognition, and the comparison key
//! under which case and Unicode aliases of one directory collide. Ports v2
//! `apps/worker/src/agents/integration-install-proof.ts`; read by
//! `agents::install_integrations` (planning) and `agents::install_transaction`
//! (commit-time revalidation), so a case alias, symlink, directory swap or
//! ownership change between the two fails closed.

use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use roost_platform::HostPlatform;
use unicode_normalization::UnicodeNormalization;

/// Why one install pass, or one target of it, did not go through.
#[derive(Debug, thiserror::Error)]
pub enum IntegrationInstallError {
    /// The installer refused: an alias, an unowned or unsafe target, a race.
    #[error("{0}")]
    Refused(String),
    /// A filesystem call failed on the named path.
    #[error("{operation} {}: {source}", .path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl IntegrationInstallError {
    pub fn refused(message: impl Into<String>) -> Self {
        Self::Refused(message.into())
    }

    /// A mapper for `map_err` that names the call and the path it touched.
    pub fn io(operation: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io {
            operation,
            path,
            source,
        }
    }

    /// Whether this is "nothing is there", which planning treats as a state
    /// rather than a failure.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}

/// A regular file as it was read. Equality is content AND identity: a file
/// rewritten in place with the same bytes is the same, a same-content file
/// swapped in under the name is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationFileSnapshot {
    pub content: String,
    pub device: u64,
    pub inode: u64,
}

/// A loader directory as it was inspected: the entry (which may be a symlink)
/// and the directory it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDirectorySnapshot {
    pub canonical_path: PathBuf,
    pub entry_is_symlink: bool,
    pub entry_device: u64,
    pub entry_inode: u64,
    pub directory_device: u64,
    pub directory_inode: u64,
}

/// One runtime's loader directory at preflight. `initial_snapshot` is `None`
/// when the directory did not exist yet; `canonical_path` is then the planned
/// resolution of the path under its deepest existing ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDirectoryPlan {
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    pub initial_snapshot: Option<IntegrationDirectorySnapshot>,
}

/// What the commit guard compares a target against. `remove` is `None` for an
/// asset (an unowned file there is a refusal) and `Some(false)` for a retired
/// file Roost will leave alone (an unowned file there is expected).
#[derive(Debug, Clone, Copy)]
pub struct IntegrationTargetPlan<'a> {
    pub target: &'a Path,
    pub ownership_marker: &'a str,
    pub existing: Option<&'a IntegrationFileSnapshot>,
    pub remove: Option<bool>,
}

/// The key under which two paths name one directory: NFC always, and case
/// folded where the host filesystem folds it.
pub fn integration_path_comparison_key(path: &Path, platform: HostPlatform) -> String {
    let normalized: String = path.to_string_lossy().nfc().collect();
    match platform {
        HostPlatform::MacOs | HostPlatform::Windows => normalized.to_lowercase(),
        HostPlatform::Linux => normalized,
    }
}

/// Ownership is a `//` comment line carrying the marker as its own
/// whitespace-delimited token, at ANY depth: an installed asset splices the
/// shared report-transport module into the integration, and a release spliced
/// it above the integration's own header, so its marker sits ~100 lines down
/// and a leading-line window would refuse the very files Roost wrote. Depth is
/// not evidence of authorship; the token is.
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

/// Snapshot a loader directory, or plan where it will resolve when it does not
/// exist yet. Any other failure — a dangling symlink, a file in the way — is
/// returned, never planned around.
pub fn preflight_integration_directory(
    path: &Path,
) -> Result<IntegrationDirectoryPlan, IntegrationInstallError> {
    match inspect_integration_directory(path) {
        Ok(snapshot) => Ok(IntegrationDirectoryPlan {
            path: path.to_path_buf(),
            canonical_path: snapshot.canonical_path.clone(),
            initial_snapshot: Some(snapshot),
        }),
        Err(error) if error.is_not_found() => Ok(IntegrationDirectoryPlan {
            path: path.to_path_buf(),
            canonical_path: canonicalize_planned_path(path)?,
            initial_snapshot: None,
        }),
        Err(error) => Err(error),
    }
}

/// Read a target that must be a regular file if it exists. A symlink or any
/// other kind of entry is refused, and a file that changed identity while it
/// was read is refused rather than trusted.
pub fn inspect_integration_target(
    path: &Path,
    description: &str,
) -> Result<Option<IntegrationFileSnapshot>, IntegrationInstallError> {
    let Some(before) = integration_lstat_if_present(path)? else {
        return Ok(None);
    };
    if before.file_type().is_symlink() {
        return Err(IntegrationInstallError::refused(format!(
            "refusing symlink {description}: {}",
            path.display()
        )));
    }
    if !before.is_file() {
        return Err(IntegrationInstallError::refused(format!(
            "refusing unsafe {description}: {}",
            path.display()
        )));
    }
    let bytes = fs::read(path).map_err(IntegrationInstallError::io("read", path))?;
    let after = fs::symlink_metadata(path).map_err(IntegrationInstallError::io("lstat", path))?;
    if !after.is_file()
        || after.file_type().is_symlink()
        || !same_integration_identity(&before, after.dev(), after.ino())
    {
        return Err(IntegrationInstallError::refused(format!(
            "{description} changed while being inspected: {}",
            path.display()
        )));
    }
    Ok(Some(IntegrationFileSnapshot {
        content: String::from_utf8_lossy(&bytes).into_owned(),
        device: after.dev(),
        inode: after.ino(),
    }))
}

/// The commit guard: the target is still exactly what planning saw, and still
/// Roost's unless the plan is to leave an unowned file alone.
pub fn assert_integration_target_unchanged(
    plan: IntegrationTargetPlan<'_>,
) -> Result<(), IntegrationInstallError> {
    let changed = || {
        IntegrationInstallError::refused(format!(
            "agent integration target changed before commit: {}",
            plan.target.display()
        ))
    };
    let current = inspect_integration_target(plan.target, "agent integration target")
        .map_err(|_| changed())?;
    if current.as_ref() != plan.existing {
        return Err(changed());
    }
    match current {
        Some(current) if !has_integration_ownership(&current.content, plan.ownership_marker) => {
            if plan.remove == Some(false) {
                return Ok(());
            }
            Err(IntegrationInstallError::refused(format!(
                "agent integration target ownership changed before commit: {}",
                plan.target.display()
            )))
        }
        _ => Ok(()),
    }
}

/// Snapshot an existing loader directory entry and the directory it resolves
/// to. An entry that resolves nowhere, or to something that is not a
/// directory, is refused.
pub fn inspect_integration_directory(
    path: &Path,
) -> Result<IntegrationDirectorySnapshot, IntegrationInstallError> {
    let entry = fs::symlink_metadata(path).map_err(IntegrationInstallError::io("lstat", path))?;
    let canonical_path = fs::canonicalize(path).map_err(|_| {
        IntegrationInstallError::refused(format!(
            "refusing dangling agent integration loader: {}",
            path.display()
        ))
    })?;
    let directory = fs::symlink_metadata(&canonical_path)
        .map_err(IntegrationInstallError::io("lstat", &canonical_path))?;
    if !directory.is_dir() {
        return Err(IntegrationInstallError::refused(format!(
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

/// The loader directory is still the one `expected` recorded: same entry, same
/// resolved directory, same canonical path under the platform's comparison.
pub fn assert_integration_directory_snapshot(
    plan: &IntegrationDirectoryPlan,
    expected: &IntegrationDirectorySnapshot,
    platform: HostPlatform,
) -> Result<(), IntegrationInstallError> {
    let changed = || {
        IntegrationInstallError::refused(format!(
            "agent integration loader changed during installation: {}",
            plan.path.display()
        ))
    };
    let current = inspect_integration_directory(&plan.path).map_err(|_| changed())?;
    if current.entry_is_symlink != expected.entry_is_symlink
        || current.entry_device != expected.entry_device
        || current.entry_inode != expected.entry_inode
        || current.directory_device != expected.directory_device
        || current.directory_inode != expected.directory_inode
        || integration_path_comparison_key(&current.canonical_path, platform)
            != integration_path_comparison_key(&expected.canonical_path, platform)
    {
        return Err(changed());
    }
    Ok(())
}

/// `lstat`, with "nothing there" as `None` rather than an error.
pub fn integration_lstat_if_present(
    path: &Path,
) -> Result<Option<Metadata>, IntegrationInstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(IntegrationInstallError::io("lstat", path)(error)),
    }
}

pub fn same_integration_identity(metadata: &Metadata, device: u64, inode: u64) -> bool {
    metadata.dev() == device && metadata.ino() == inode
}

/// `path` with `.` and `..` resolved lexically and separators collapsed, the
/// way a `path.join` result reads: the extension directories come from
/// operator-set variables, and the reported and compared paths are that form.
pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut named_depth = 0usize;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir if named_depth > 0 => {
                normalized.pop();
                named_depth -= 1;
            }
            Component::ParentDir if normalized.has_root() => {}
            Component::ParentDir => normalized.push(".."),
            Component::Normal(name) => {
                normalized.push(name);
                named_depth += 1;
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    normalized
}

/// Where an absent directory will resolve: its deepest existing ancestor's
/// real path, with the missing tail appended as written.
fn canonicalize_planned_path(path: &Path) -> Result<PathBuf, IntegrationInstallError> {
    let absolute = if path.is_absolute() {
        normalize_lexically(path)
    } else {
        let cwd = std::env::current_dir().map_err(IntegrationInstallError::io("getcwd", path))?;
        normalize_lexically(&cwd.join(path))
    };
    canonicalize_existing_ancestor(&absolute)
}

fn canonicalize_existing_ancestor(absolute: &Path) -> Result<PathBuf, IntegrationInstallError> {
    match fs::canonicalize(absolute) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match (absolute.parent(), absolute.file_name()) {
                (Some(parent), Some(name)) => {
                    Ok(canonicalize_existing_ancestor(parent)?.join(name))
                }
                _ => Err(IntegrationInstallError::io("realpath", absolute)(error)),
            }
        }
        Err(error) => Err(IntegrationInstallError::io("realpath", absolute)(error)),
    }
}
