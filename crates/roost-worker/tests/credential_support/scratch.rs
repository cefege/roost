//! A scratch directory no two tests share, and no test leaves behind.
//!
//! The credential and install fixtures both need a real directory — a real
//! mode on a real key file, a real service definition a real rename can move —
//! so they cannot be faked into a `MapEnv` value. This is the whole fixture
//! they share.

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
