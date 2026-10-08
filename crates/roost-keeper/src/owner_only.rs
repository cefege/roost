//! Directories and files only their owner may open: the capability, the pid
//! file and the socket. Called by `capability`, `server` and the daemon binary.
//! On Unix the mode bits say so (0700 directories, 0600 files). On Windows
//! these live under `%LOCALAPPDATA%`, whose inherited ACL already limits access
//! to the user, SYSTEM and Administrators, so no per-file change is made.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::path::Path;

/// Create `path` and every missing parent, owner-only.
#[cfg(unix)]
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

/// Create `path` and every missing parent, owner-only.
#[cfg(windows)]
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).create(path)
}

/// Create `path`, failing when it exists, owner-only from the first byte.
#[cfg(unix)]
pub fn create_new_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Create `path`, failing when it exists, owner-only from the first byte.
#[cfg(windows)]
pub fn create_new_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Create or truncate `path`, owner-only from the first byte.
#[cfg(unix)]
pub fn create_truncate_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Create or truncate `path`, owner-only from the first byte.
#[cfg(windows)]
pub fn create_truncate_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// State the owner-only mode exactly: the mode at creation is filtered by the
/// umask.
#[cfg(unix)]
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// The inherited `%LOCALAPPDATA%` ACL is the restriction; nothing to change.
#[cfg(windows)]
pub fn restrict_to_owner(_path: &Path) -> io::Result<()> {
    Ok(())
}
