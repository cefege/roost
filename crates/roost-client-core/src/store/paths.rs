//! The platform capability the navigation projection needs: what two paths on
//! one worker mean.
//!
//! `roost-platform` owns the path codec (`native_path_identity_key`,
//! `native_path_basename`, `same_worker_folder`) and the crate DAG forbids
//! `roost-client-core` from depending on it, so the capability arrives as a trait
//! the host implements and this crate calls. What this module must never do is
//! re-implement path handling: two spellings of one directory that stay apart put
//! two of a user's terminals in two folder tabs, and folding two real directories
//! together is a merge no UI can undo.
//!
//! `ExactWorkerPaths` below is the in-crate test double, deliberately the
//! STRICTEST implementation — the platform codec's own tests pin the macOS
//! `/tmp` → `/private/tmp` folding, so a core test that leaned on them would be
//! testing another crate through a window. What a core test needs is the
//! guarantee that these two methods never merge or mangle two paths the host
//! would keep apart, and exact equality has that by construction.
//!
//! Depends on nothing but the trait's own parameters.

/// `Debug` because every type that HOLDS a codec is a public store type and the
/// workspace denies a public type without one; a host implementation gets it
/// from `#[derive(Debug)]` on a unit struct, as the double below does.
///
pub trait WorkerPaths: std::fmt::Debug {
    /// The stable key two spellings of one directory share, or `None` when this
    /// path cannot be normalized at all.
    ///
    /// `None` is not permission to share a placeholder: a path with no key
    /// buckets with itself and nothing else.
    fn folder_key(&self, worker_os: Option<&str>, path: &str) -> Option<String>;

    /// The last segment of a path — what a folder row shows, and what a session
    /// with no title of its own is called.
    ///
    /// `None` when it cannot be computed, which the caller must read as "show
    /// the whole path" rather than as an empty label.
    fn basename(&self, worker_os: Option<&str>, path: &str) -> Option<String>;

    /// Whether two paths name one directory.
    ///
    /// A pair where either side cannot normalize falls back to EXACT equality,
    /// never to a guess. `roost-platform`'s `same_worker_folder` makes the same
    /// choice for the same reason, and it is the one place that choice is
    /// allowed to be written down.
    fn same_folder(&self, worker_os: Option<&str>, left: &str, right: &str) -> bool {
        match (
            self.folder_key(worker_os, left),
            self.folder_key(worker_os, right),
        ) {
            (Some(left_key), Some(right_key)) => left_key == right_key,
            _ => left == right,
        }
    }
}

/// A path codec that folds nothing: exact string equality, and no basename.
///
/// The strictest implementation, for the reason in the module header. `basename`
/// answering `None` is what forces a caller to have a whole-path fallback, so a
/// core test cannot accidentally depend on a segment split this crate does not
/// own.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExactWorkerPaths;

impl WorkerPaths for ExactWorkerPaths {
    fn folder_key(&self, _worker_os: Option<&str>, path: &str) -> Option<String> {
        Some(path.to_owned())
    }

    fn basename(&self, _worker_os: Option<&str>, _path: &str) -> Option<String> {
        None
    }
}

/// The stable per-(worker, folder) key the sidebar and the pane deck bucket by.
///
/// `raw::` marks a path the platform could not normalize. It is in the key rather
/// than only in the fallback so a raw path can never collide with a normalized one
/// that happens to carry the same text.
pub fn folder_key_of(
    paths: &dyn WorkerPaths,
    worker_os: Option<&str>,
    worker_fp: &str,
    path: &str,
) -> String {
    match paths.folder_key(worker_os, path) {
        Some(key) => format!("{worker_fp}::{key}"),
        None => format!("{worker_fp}::raw::{path}"),
    }
}
