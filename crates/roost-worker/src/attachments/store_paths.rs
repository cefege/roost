//! Where attachments live: the base every session's directory hangs from, one
//! session's directory inside it, and the private names a session directory
//! holds beside its files. Ports the path half of v2
//! `apps/worker/src/attachments/attachment-reaper.ts` (`attachmentBaseDir`,
//! `attachmentSessionDir`, `resolveSessionDirWithinBase`, the private names).
//! Called by the file store, the operation journal, the reaper and the browser
//! attachment commands.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The dedup index (digest → filename) in each session directory. It survives
/// the reaper and never appears in a listing.
pub const MANIFEST_NAME: &str = ".roost-manifest.json";

/// Durable operation state (`<upload>.json`, `<upload>.part`), private to the
/// operation owner.
pub const ATTACHMENT_OPERATION_DIR_NAME: &str = ".operations";

/// The `p1`, `p2`, … links a short-path upload is answered with.
pub const SHORTCUT_DIR_NAME: &str = ".shortcuts";

/// The attachment base, resolved once: `<worker data dir>/attachments` (v2
/// used `~/.roost/attachments`; see the crate README's deviations). Every
/// session directory is derived from it, and nothing outside it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentBase {
    root: PathBuf,
}

impl AttachmentBase {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: resolve_lexically(&root.into()),
        }
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
        Some(dir)
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

#[cfg(test)]
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
