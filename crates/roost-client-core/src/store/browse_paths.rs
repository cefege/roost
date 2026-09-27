//! The path arithmetic a machine-scoped file browser needs, as a trait the host
//! implements.
//!
//! `roost-platform` owns the canonical path codec and the crate DAG forbids
//! `roost-client-core` from depending on it, so the arithmetic arrives the same
//! way `store::paths::FolderIdentity` arrives: as a capability the host
//! provides. What this file must never do is re-implement normalization. Two
//! spellings of one directory that stay apart put two of a viewer's terminals
//! in two folders, and folding two real directories together is a merge no UI
//! can undo.
//!
//! Deliberately two verbs, not six. `child` and `parent` are what the browse
//! STATE needs to move; breadcrumbs, labels and titles are the host's own
//! rendering of a path it already has, and a second seam for them would be a
//! second answer to "how is this path spelled".

/// Canonical path arithmetic on one machine.
pub trait BrowsePathOps {
    /// The path of `name` inside `dir`.
    ///
    /// A name that is itself a path, or that is `.` or `..`, is the host
    /// codec's business: this is where a listing row's name is turned into a
    /// navigable path, and only the codec knows whether the machine's shell
    /// would treat it as one.
    fn child(&self, dir: &str, name: &str) -> String;

    /// The directory containing `dir`.
    ///
    /// A root, and the browse `~` sentinel, must return themselves: "up" from
    /// the top of a machine's filesystem is the top of that filesystem, and a
    /// `..` that escapes it is not a directory a listing can be asked for.
    fn parent(&self, dir: &str) -> String;
}

/// The browse `~` sentinel: the machine's home directory, as a canonical path.
///
/// `~` is what a session's `cwd` is stored as when the machine reported a home
/// directory, and it is the one path a browser can show before the first listing
/// has resolved anything.
pub const BROWSE_HOME: &str = "~";

/// A codec that folds nothing and escapes nothing: `/`-joined POSIX strings.
///
/// The in-crate test double, and the strictest implementation available. The
/// host's real codec is `roost_platform::native_path`, and a test that leaned
/// on its macOS `/tmp` → `/private/tmp` folding would be testing the platform
/// crate's behaviour from here — which is what
/// `store::paths::ExactFolderIdentity` refuses to do for the same reason.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExactBrowsePaths;

impl BrowsePathOps for ExactBrowsePaths {
    fn child(&self, dir: &str, name: &str) -> String {
        // The sentinel JOINS like any other segment: v2's `nativePathJoin`
        // normalises `~` and then appends with a separator, so a child of home
        // is `~/src` and not `src`. Dropping the sentinel here made the child
        // a bare relative name, and one `..` away from `parent()` — which
        // answers `/` for anything unanchored — so going up out of a folder
        // opened from home landed on the filesystem root instead of on `~`.
        if dir.is_empty() {
            return name.to_owned();
        }
        if dir.ends_with('/') {
            format!("{dir}{name}")
        } else {
            format!("{dir}/{name}")
        }
    }

    fn parent(&self, dir: &str) -> String {
        if dir.is_empty() || dir == BROWSE_HOME {
            return dir.to_owned();
        }
        let trimmed = dir.trim_end_matches('/');
        match trimmed.rsplit_once('/') {
            Some(("", _)) | None => "/".to_owned(),
            Some((parent, _)) => parent.to_owned(),
        }
    }
}
