//! How a fresh keeper process is detached from the worker so it outlives it,
//! and the owner-only log its stderr goes to. Called by `keeper_boot`'s
//! `start_fresh_keeper`. Unix puts the keeper in its own process group; Windows
//! detaches it from the console, gives it its own process group, and breaks it
//! out of the service's job object when the job allows that.

use std::path::Path;

/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`.
#[cfg(windows)]
const KEEPER_CREATION_FLAGS: u32 = 0x0000_0008 | 0x0000_0200 | KEEPER_BREAKAWAY_FLAG;

/// `CREATE_BREAKAWAY_FROM_JOB`: a Task Scheduler job ends every member when the
/// task stops, and a keeper inside it would take every PTY with it.
#[cfg(windows)]
const KEEPER_BREAKAWAY_FLAG: u32 = 0x0100_0000;

/// Windows' `ERROR_ACCESS_DENIED`, which `CreateProcess` returns when the
/// parent's job forbids breakaway.
#[cfg(windows)]
const ACCESS_DENIED: i32 = 5;

/// Spawn `command` detached from this process's lifetime.
#[cfg(unix)]
pub(super) fn spawn_detached_keeper(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    // Its own process group: launchd ends a job by signalling the job's
    // whole process group, so a keeper left in the worker's group dies with
    // every worker restart and takes each PTY with it.
    command.process_group(0);
    command.spawn()
}

/// Spawn `command` detached from this process's lifetime.
#[cfg(windows)]
pub(super) fn spawn_detached_keeper(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    command.creation_flags(KEEPER_CREATION_FLAGS);
    match command.spawn() {
        Err(error) if error.raw_os_error() == Some(ACCESS_DENIED) => {
            tracing::warn!(
                "keeper started inside the service job; ending the task will end the keeper"
            );
            command.creation_flags(KEEPER_CREATION_FLAGS & !KEEPER_BREAKAWAY_FLAG);
            command.spawn()
        }
        spawned => spawned,
    }
}

/// The keeper's own log, owner-readable only.
///
/// A keeper log carries the output of every shell on the machine, so the mode is
/// set at creation rather than tightened afterwards. On Windows it lives under
/// `%LOCALAPPDATA%`, whose inherited ACL is the restriction.
pub(super) fn keeper_log_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    options.open(path)
}
