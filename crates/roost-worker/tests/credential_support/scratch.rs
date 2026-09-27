//! A scratch directory no two tests share, and no test leaves behind.
//!
//! The credential and install fixtures both need a real directory — a real
//! mode on a real key file, a real service definition a real rename can move —
//! so they cannot be faked into a `MapEnv` value. This is the whole fixture
//! they share.

// NINE test binaries include this module by `#[path]`, and each one calls a
// different subset of it: `path` is called by seven, `root` by three
// (`host_folder_facts`, `shell_spec_resolution`, `worker_retire_authorization`),
// and the other four call neither. So a dead-code warning here is a statement
// about ONE binary, not about the fixture, and deleting or narrowing a method
// to quiet one would break a caller in a different binary. That is the
// asymmetry worth carrying: for a symbol in a private support module, "make it
// private and see if it still builds" proves nothing, because a `pub` in a
// private module is already unreachable outside the crate. The check that does
// carry information is the opposite trade — and it was run: this allow was
// absent, all nine binaries were compiled, and the per-binary lint lists
// (`root` in five of them, `path` in two) were read against that map.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// One directory, removed when the value goes out of scope.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A fresh directory named after the fixture that asked for it.
    ///
    /// The pid and an ordinal are in the name because the tests run on parallel
    /// threads inside one process: two fixtures sharing a root would be two
    /// tests writing over each other's key files.
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "roost-credential-{label}-{}-{ordinal}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap_or_else(|error| {
            panic!("the scratch root {} is unusable: {error}", root.display())
        });
        Self { root }
    }

    /// The root itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path inside the root, whether or not anything is there yet.
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A test that failed mid-write may have left a key behind, and a
        // leftover private key in /tmp is exactly the thing this slice exists
        // to avoid leaving around. The cleanup is best effort because a failure
        // here must not mask the assertion that already failed.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
