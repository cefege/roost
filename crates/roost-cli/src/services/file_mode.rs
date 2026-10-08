//! Unix permission bits and directory flushes, in the one place roost-cli
//! touches them. Called by the atomic writer, the installer, the release
//! appliers and the self-update rollout. On Windows a file's protection is the
//! ACL of `%LOCALAPPDATA%`, a directory cannot be flushed, and an executable is
//! named by its extension, so every operation here is a no-op that succeeds.

use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

/// A file's permission bits (`0o7777`).
#[cfg(unix)]
pub fn mode_of(metadata: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o7777
}

/// No permission bits on Windows.
#[cfg(windows)]
pub fn mode_of(_metadata: &Metadata) -> u32 {
    0
}

/// Set an open file's permission bits.
#[cfg(unix)]
pub fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// No permission bits on Windows.
#[cfg(windows)]
pub fn set_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Set a path's permission bits.
#[cfg(unix)]
pub fn set_path_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// No permission bits on Windows.
#[cfg(windows)]
pub fn set_path_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Flush a directory's entries, so a rename into it survives a crash.
#[cfg(unix)]
pub fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// NTFS journals directory entries itself; a directory handle cannot be
/// flushed on Windows.
#[cfg(windows)]
pub fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Whether two permission readings agree. Windows reads none, so they always do.
#[cfg(unix)]
pub const fn modes_match(left: u32, right: u32) -> bool {
    left == right
}

/// Whether two permission readings agree. Windows reads none, so they always do.
#[cfg(windows)]
pub const fn modes_match(_left: u32, _right: u32) -> bool {
    true
}
