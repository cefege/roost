//! Where attachments live: the private base every session's directory hangs
//! from, one session's directory inside it, the private names a directory
//! holds beside its files, and the media directory a session's uploads land
//! in — `<folder>/.roost/media` when its shell stands in a folder, else the
//! private session directory. Ports the path half of v2
//! `apps/worker/src/attachments/attachment-reaper.ts`. Called by the file
//! store, the operation journal, the reaper and the browser attachment
//! commands; the project half is `media_dirs`.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use super::media_dirs::{
    MediaDirRegistry, SessionFolders, prepare_project_media_dir, project_media_dir,
};

/// The dedup index (digest → filename) in each session directory. It survives
/// the reaper and never appears in a listing.
pub const MANIFEST_NAME: &str = ".roost-manifest.json";

/// Durable operation state (`<upload>.json`, `<upload>.part`), private to the
/// operation owner.
pub const ATTACHMENT_OPERATION_DIR_NAME: &str = ".operations";

/// The `p1`, `p2`, … links a short-path upload is answered with.
pub const SHORTCUT_DIR_NAME: &str = ".shortcuts";

/// The attachment base, resolved once: `<worker data dir>/attachments` (v2
/// used `~/.roost/attachments`; see the crate README's deviations). Operation
/// journals always live beneath it; uploaded files land in [`Self::media_dir`].
#[derive(Clone)]
pub struct AttachmentBase {
    root: PathBuf,
    folders: Option<Arc<dyn SessionFolders>>,
    registry: Arc<MediaDirRegistry>,
}

impl std::fmt::Debug for AttachmentBase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentBase")
            .field("root", &self.root)
            .field("project_media", &self.folders.is_some())
            .finish()
    }
}

impl AttachmentBase {
    /// A base whose uploads all land in private session directories.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = resolve_lexically(&root.into());
        Self {
            registry: Arc::new(MediaDirRegistry::new(&root)),
            root,
            folders: None,
        }
    }

    /// Land each session's uploads in its shell's folder, under `.roost/media`.
    #[must_use]
    pub fn with_session_folders(mut self, folders: Arc<dyn SessionFolders>) -> Self {
        self.folders = Some(folders);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// v2 `attachmentSessionDir`: the base joined with the id, unchecked.
    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        join_lexically(&self.root, session_id)
    }

    /// v2 `resolveSessionDirWithinBase`: the session's directory, or `None`
    /// for an id such as `../../etc` that climbs out of the base. The id is
    /// joined as text and folded without touching the filesystem, exactly as
    /// Node's `path.resolve` does, and the base itself counts as inside.
    pub fn resolve_session_dir(&self, session_id: &str) -> Option<PathBuf> {
        let dir = self.session_dir(session_id);
        dir.starts_with(&self.root).then_some(dir)
    }

    /// Where this session's uploads land now, without creating anything: the
    /// project media directory of its shell's folder, else its private one.
    pub fn media_dir(&self, session_id: &str) -> Option<PathBuf> {
        self.resolve_session_dir(session_id)?;
        self.project_media_dir(session_id)
            .or_else(|| self.resolve_session_dir(session_id))
    }

    /// [`Self::media_dir`], created and registered for an upload about to
    /// write into it. A project directory that cannot be prepared falls back
    /// to the private one rather than failing the upload.
    pub fn prepare_media_dir(&self, session_id: &str) -> Option<PathBuf> {
        let private = self.resolve_session_dir(session_id)?;
        let Some(dir) = self.project_media_dir(session_id) else {
            return Some(private);
        };
        match prepare_project_media_dir(&dir).and_then(|()| self.registry.register(&dir)) {
            Ok(()) => Some(dir),
            Err(error) => {
                tracing::warn!(
                    session_id,
                    dir = %dir.display(),
                    %error,
                    "the project media directory could not be prepared; the upload lands in the private session directory"
                );
                Some(private)
            }
        }
    }

    /// The project media directories the reaper bounds beside the base.
    pub fn media_registry(&self) -> &MediaDirRegistry {
        &self.registry
    }

    /// A folder inside the base is never a project: a shell standing in the
    /// worker's own data must not grow a `.roost` there.
    fn project_media_dir(&self, session_id: &str) -> Option<PathBuf> {
        let folder = self.folders.as_ref()?.session_folder(session_id)?;
        let usable = folder.is_absolute() && !folder.starts_with(&self.root) && folder.is_dir();
        usable.then(|| project_media_dir(&normalize(&folder)))
    }
}

/// Node's `path.resolve` of one path: absolute against the current directory,
/// `.` and `..` folded lexically.
pub fn resolve_lexically(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return normalize(path);
    }
    match std::env::current_dir() {
        Ok(cwd) => normalize(&cwd.join(path)),
        Err(_) => normalize(path),
    }
}

/// Node's `path.join(base, tail)`: the tail is appended as TEXT — an absolute
/// tail does not replace the base, as `PathBuf::join` would — then folded.
pub fn join_lexically(base: &Path, tail: &str) -> PathBuf {
    let mut joined = OsString::from(base.as_os_str());
    joined.push("/");
    joined.push(tail);
    normalize(Path::new(&joined))
}

fn normalize(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => folded.push(component.as_os_str()),
            Component::CurDir => {}
            // At the root this is a no-op, as `path.resolve("/..")` is "/".
            Component::ParentDir => {
                folded.pop();
            }
            Component::Normal(part) => folded.push(part),
        }
    }
    folded
}

// The paths asserted are POSIX ones.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_that_climbs_out_of_the_base_has_no_directory() {
        let base = AttachmentBase::new("/home/user/.roost/attachments");
        assert_eq!(base.resolve_session_dir("../../etc"), None);
        assert_eq!(base.resolve_session_dir("../attachments-evil"), None);
        assert_eq!(
            base.resolve_session_dir("/etc"),
            Some(PathBuf::from("/home/user/.roost/attachments/etc")),
            "an absolute id is appended as text, never substituted for the base"
        );
        assert_eq!(
            base.resolve_session_dir("a/../b"),
            Some(PathBuf::from("/home/user/.roost/attachments/b"))
        );
        assert_eq!(
            base.resolve_session_dir(""),
            Some(PathBuf::from("/home/user/.roost/attachments")),
            "v2 counts the base itself as inside"
        );
    }
}
