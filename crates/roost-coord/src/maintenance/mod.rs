//! The coordinator's own housekeeping: the nightly database backup, the
//! audit_log retention sweep, and the boot-time janitor.
//!
//! Owned by the coordinator. None of it is a Connect method. Each piece takes
//! `&CoordDb` and either writes rows or writes files; the `spawn_*` entry
//! points hand that handle to a long-lived task and return immediately, which
//! is the whole of what v2's `setInterval(…).unref()` bought -- a tokio task
//! does not keep the process alive, so there is nothing to unref.

pub mod audit_retention;
pub mod backup;
pub mod gzip_file;
pub mod snapshot;
pub mod startup_janitor;

use std::path::Path;

/// Remove a file if it is there, and treat "already gone" as success.
///
/// Every cleanup in this module runs on the failure path of something else, so
/// a missing file is the desired end state rather than an error worth
/// reporting -- and reporting it would mask the failure that brought us here.
pub(crate) async fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Set a file's mode to `0o600`, whatever the process umask allowed at creation.
///
/// Set explicitly rather than inherited: a database archive must not be
/// readable by another user for even the length of one write.
pub(crate) async fn set_owner_only_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

/// Set a directory's mode to `0o700`, whatever the process umask allowed.
pub(crate) async fn set_owner_only_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}
