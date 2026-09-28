//! Cumulative terminal counts per folder for the folder picker: how many shell
//! sessions on one machine live in each listed folder or anywhere beneath it.
//! Ports `apps/web/src/lib/folderActivity.ts`; the browse picker reads it.
//! Path identity is the host's `WorkerPaths` codec.

use std::collections::BTreeMap;

use roost_protocol::wire::{Session, SessionKind};

use crate::store::paths::WorkerPaths;

/// One folder's activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderActivity {
    /// Sessions whose cwd is this folder or any descendant.
    pub terminals: usize,
}

/// Count, for each of `folder_paths` on `worker_fp`, the shell sessions whose
/// cwd is that folder or beneath it. Keyed by the input path verbatim; a folder
/// with none is absent.
///
/// Identity keys preserve POSIX case, fold Windows case, and use `/`; the
/// separator suffix keeps `C:/work` from claiming `C:/worker`.
pub fn compute_folder_activity(
    sessions: &[&Session],
    paths: &dyn WorkerPaths,
    worker_os: Option<&str>,
    worker_fp: &str,
    folder_paths: &[&str],
) -> BTreeMap<String, FolderActivity> {
    let identity = |path: &str| {
        paths
            .folder_key(worker_os, path)
            .unwrap_or_else(|| path.to_owned())
    };
    let machine_cwds: Vec<String> = sessions
        .iter()
        .filter(|session| {
            session.kind == SessionKind::Shell && session.worker_fp.as_str() == worker_fp
        })
        .map(|session| identity(&session.cwd))
        .collect();
    let mut activity = BTreeMap::new();
    for folder in folder_paths {
        let base = identity(folder);
        let prefix = if base.ends_with('/') {
            base.clone()
        } else {
            format!("{base}/")
        };
        let terminals = machine_cwds
            .iter()
            .filter(|cwd| **cwd == base || cwd.starts_with(&prefix))
            .count();
        if terminals > 0 {
            activity.insert((*folder).to_owned(), FolderActivity { terminals });
        }
    }
    activity
}
