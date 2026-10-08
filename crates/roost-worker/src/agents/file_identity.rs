//! The `(device, inode)` identity the agent-integration installer proves a
//! file or directory by: `(st_dev, st_ino)` on Unix, `(volume serial, file
//! index)` on Windows through `roost_keeper::win32_ffi`. Called by
//! `install_proof`, `install_stage` and `install_mutation`.

use std::io;
use std::path::Path;

/// The identity of `path`; `follow_links = false` identifies a link itself.
#[cfg(unix)]
pub(crate) fn path_identity(path: &Path, follow_links: bool) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = if follow_links {
        std::fs::metadata(path)?
    } else {
        std::fs::symlink_metadata(path)?
    };
    Ok((metadata.dev(), metadata.ino()))
}

/// The identity of `path`; `follow_links = false` identifies a link itself.
#[cfg(windows)]
pub(crate) fn path_identity(path: &Path, follow_links: bool) -> io::Result<(u64, u64)> {
    roost_keeper::win32_ffi::path_identity(path, follow_links)
}
