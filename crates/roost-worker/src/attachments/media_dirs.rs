//! Where a session's uploads land when its shell stands in a folder:
//! `<folder>/.roost/media`, kept out of git by a `.gitignore` written inside
//! it, and remembered in a registry under the attachment base so the reaper
//! still bounds a folder no live session points at. Called by
//! `store_paths::AttachmentBase` and the reaper; the session layer answers
//! the folder through [`SessionFolders`].

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use super::file_store::{create_private_dir, write_private_file};

/// The `.gitignore` every project media directory carries. The reaper and
/// the browser listing both skip it.
pub const MEDIA_GITIGNORE_NAME: &str = ".gitignore";

/// `*` ignores the directory's whole content, this file included, so a
/// project never shows a Roost upload in `git status`.
const MEDIA_GITIGNORE: &str = "# Roost uploads; the worker deletes them after 7 days.\n*\n";

/// The registry file beneath the attachment base.
pub const MEDIA_REGISTRY_NAME: &str = "media-dirs.json";

/// The folder a session's shell is in now. Implemented by the session table;
/// a test supplies a fixed answer.
pub trait SessionFolders: Send + Sync {
    /// `None` for a session this worker does not hold.
    fn session_folder(&self, session_id: &str) -> Option<PathBuf>;
}

/// `<folder>/.roost/media`.
pub fn project_media_dir(folder: &Path) -> PathBuf {
    folder.join(".roost").join("media")
}

/// Whether `dir` has a project media directory's shape. The reaper deletes
/// outside the base only beneath such a directory, whatever the registry says.
pub fn is_project_media_dir(dir: &Path) -> bool {
    dir.is_absolute() && dir.ends_with(".roost/media")
}

/// Create the directory and its `.gitignore`. An existing `.gitignore` is the
/// user's and is left alone. A `.roost` or `media` that is a symlink is
/// refused: the reaper deletes inside this directory, and a cloned repository
/// must not be able to aim it somewhere else.
pub fn prepare_project_media_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        create_private_dir(parent)?;
    }
    create_private_dir(dir)?;
    if !is_real_media_dir(dir) {
        return Err(io::Error::other(
            "the project media directory or its .roost parent is a symlink",
        ));
    }
    let gitignore = dir.join(MEDIA_GITIGNORE_NAME);
    if !gitignore.exists() {
        write_private_file(&gitignore, MEDIA_GITIGNORE.as_bytes())?;
    }
    Ok(())
}

/// Whether `dir` and its `.roost` parent are directories in their own right,
/// not symlinks to one.
pub fn is_real_media_dir(dir: &Path) -> bool {
    let real = |path: &Path| fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir());
    real(dir) && dir.parent().is_some_and(real)
}

/// Every project media directory this worker has written into, persisted as a
/// JSON array of absolute paths. Loaded once, written only when it changes.
#[derive(Debug)]
pub struct MediaDirRegistry {
    path: PathBuf,
    known: Mutex<Option<BTreeSet<PathBuf>>>,
}

impl MediaDirRegistry {
    pub fn new(base_root: &Path) -> Self {
        Self {
            path: base_root.join(MEDIA_REGISTRY_NAME),
            known: Mutex::new(None),
        }
    }

    /// Remember `dir`. Durable before it returns, so an upload never lands in
    /// a directory the reaper cannot find.
    pub fn register(&self, dir: &Path) -> io::Result<()> {
        let mut known = self.lock_loaded();
        let set = known.get_or_insert_with(BTreeSet::new);
        if set.contains(dir) {
            return Ok(());
        }
        let mut next = set.clone();
        next.insert(dir.to_path_buf());
        self.persist(&next)?;
        *set = next;
        tracing::info!(dir = %dir.display(), "a project media directory was registered");
        Ok(())
    }

    /// The registered directories, in path order.
    pub fn registered(&self) -> Vec<PathBuf> {
        self.lock_loaded()
            .as_ref()
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Drop `dir`, which no longer exists.
    pub fn forget(&self, dir: &Path) -> io::Result<()> {
        let mut known = self.lock_loaded();
        let Some(set) = known.as_mut() else {
            return Ok(());
        };
        if !set.contains(dir) {
            return Ok(());
        }
        let mut next = set.clone();
        next.remove(dir);
        self.persist(&next)?;
        *set = next;
        tracing::info!(dir = %dir.display(), "a vanished project media directory was forgotten");
        Ok(())
    }

    fn lock_loaded(&self) -> MutexGuard<'_, Option<BTreeSet<PathBuf>>> {
        let mut known = self
            .known
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if known.is_none() {
            *known = Some(load_registry(&self.path));
        }
        known
    }

    fn persist(&self, dirs: &BTreeSet<PathBuf>) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            create_private_dir(parent)?;
        }
        let listed: Vec<String> = dirs
            .iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect();
        let bytes = serde_json::to_vec(&listed).map_err(io::Error::other)?;
        let mut pending = self.path.clone().into_os_string();
        pending.push(".next");
        let pending = PathBuf::from(pending);
        write_private_file(&pending, &bytes)?;
        fs::File::open(&pending)?.sync_all()?;
        fs::rename(&pending, &self.path)
    }
}

/// A missing or corrupt registry is empty: the worst it costs is a directory
/// the reaper no longer visits, never a deletion outside one.
fn load_registry(path: &Path) -> BTreeSet<PathBuf> {
    let listed: Vec<String> = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    listed
        .into_iter()
        .map(PathBuf::from)
        .filter(|dir| is_project_media_dir(dir))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_dot_roost_media_directory_is_a_project_media_directory() {
        assert!(is_project_media_dir(Path::new("/repo/.roost/media")));
        assert!(!is_project_media_dir(Path::new("/repo/.roost")));
        assert!(!is_project_media_dir(Path::new("/repo/media")));
        assert!(!is_project_media_dir(Path::new("repo/.roost/media")));
        assert!(!is_project_media_dir(Path::new("/repo/.roost/media/sub")));
    }
}
