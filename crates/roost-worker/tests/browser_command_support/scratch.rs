//! A scratch directory no two tests share.
//!
//! The tests run in parallel threads inside one process, so a shared root
//! would have them writing over each other's fixtures.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

pub fn scratch_root() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "roost-browser-commands-{}-{ordinal}",
        std::process::id()
    ))
}
