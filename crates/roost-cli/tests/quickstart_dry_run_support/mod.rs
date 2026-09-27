//! The fixture both quickstart dry-run test binaries share: a throwaway machine
//! with a home directory and a data root, and a whole-tree snapshot of it.
//!
//! Split out rather than declared twice because the snapshot is the comparison
//! that decides "a dry run wrote nothing", and two copies of it are two
//! definitions of what a file change is.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use roost_cli::quickstart::endpoint::QuickstartEndpoint;
pub use roost_cli::quickstart::endpoint::fresh_endpoint;
use roost_host::MapEnv;

/// A throwaway tree that removes itself, standing in for a machine with no
/// install of any kind.
pub struct TempMachine {
    /// The whole throwaway tree: home, data root and everything either creates.
    pub root: PathBuf,
}

impl TempMachine {
    pub fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-quickstart-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway machine is created");
        Self { root }
    }

    /// An account with a home and nothing else under it.
    /// The tree's root, for a test that needs to build a fixture inside it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn environment(&self) -> MapEnv {
        let text = |relative: &str| self.root.join(relative).display().to_string();
        MapEnv::new()
            .with("HOME", &text("home"))
            .with(
                roost_host::COORD_UNIT_ENV,
                &text("unit/roost3-coord.service"),
            )
            .with(
                roost_host::WORKER_UNIT_ENV,
                &text("unit/roost3-worker.service"),
            )
            .with(roost_host::WORKER_DATA_DIR_ENV, &text("data/worker"))
            .with(roost_host::COORD_DATA_DIR_ENV, &text("data/coord"))
    }
}

impl Drop for TempMachine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every file under `root` as its bytes and permission bits, keyed by its path
/// relative to `root`. This is the whole comparison: a file that appeared, a
/// directory that was created, a byte that changed.
pub fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut snapshot = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("every entry is under the root")
                .to_path_buf();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(path);
            } else {
                snapshot.insert(
                    relative,
                    (
                        std::fs::read(&path).unwrap_or_default(),
                        metadata.permissions().mode() & 0o7777,
                    ),
                );
            }
        }
    }
    snapshot
}
