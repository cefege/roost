//! The temporary copies `/api/db-export` streams, and the two bounds that stop
//! them from filling the disk.
//!
//! Owned by `http::listener`, which serves the export route. v2's
//! `_sweepExportSnapshots` and its per-response expiry timer
//! (`bun-coordinator-listeners.ts:49-95, 206-252`), ported whole: an export is
//! a FULL COPY of the database, so a count bound alone lets a caller pin disk
//! with concurrent downloads, and an age bound alone lets N exports inside one
//! window do it. Both bounds run before a new copy is taken, so the keep count
//! always has room for the one being made.
//!
//! THE FILE OUTLIVES THE RESPONSE. The body is streamed from disk after the
//! handler returns, so unlinking at the end of the request truncates the
//! download. The reclaim is a timer on a detached task, never an abort listener:
//! a cancelled download is exactly the event that would fire one, and v2's note
//! records what that path cost.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::db::CoordDb;
use crate::maintenance::snapshot::create_sqlite_snapshot;

/// The name an export copy takes. Leading dot, so it is never mistaken for a
/// database by anything that globs the data directory.
pub const EXPORT_SNAPSHOT_PREFIX: &str = ".coord-export-";

/// The suffix, and what makes a sweep's name filter total.
pub const EXPORT_SNAPSHOT_SUFFIX: &str = ".db";

/// How long one export copy may sit on disk. v2's `EXPORT_SNAPSHOT_TTL_MS`:
/// long enough for a multi-hundred-megabyte download over a slow link, short
/// enough that an abandoned one is not a disk incident by morning.
pub const EXPORT_SNAPSHOT_TTL: Duration = Duration::from_secs(15 * 60);

/// Newest export copies a directory may keep. v2's
/// `EXPORT_SNAPSHOT_MAX_RESIDENT`, and each one is a full database.
pub const EXPORT_SNAPSHOT_MAX_RESIDENT: usize = 2;

/// The download name the browser is offered. v2 hard-codes the v2 generation's
/// filename and so does this: the file IS a coordinator database, and the
/// import command reads it whatever it is called.
pub const EXPORT_DOWNLOAD_NAME: &str = "coordinator_v2.db";

/// One export copy, named for the response that is streaming it.
#[derive(Debug, Clone)]
pub struct ExportSnapshot {
    /// Where the copy is.
    pub path: PathBuf,
    /// Its size in bytes, which is also the response's `content-length`.
    pub size: u64,
}

/// A consistent copy for one download, with the previous ones swept first.
///
/// The sweep runs with [`EXPORT_SNAPSHOT_MAX_RESIDENT`] MINUS ONE kept, so the
/// copy made here is the last one the count bound allows. Taking the copy first
/// and sweeping after would briefly allow one more than the bound states, which
/// is the direction a disk bound must not err in.
pub async fn prepare_export_snapshot(database: &CoordDb) -> Result<ExportSnapshot, ExportError> {
    let directory = database
        .sqlite_path()
        .ok_or(ExportError::NotAFile)?
        .parent()
        .ok_or(ExportError::NoDirectory)?
        .to_path_buf();
    let pruned = sweep_export_snapshots(
        &directory,
        EXPORT_SNAPSHOT_TTL,
        EXPORT_SNAPSHOT_MAX_RESIDENT - 1,
    )
    .await;
    if pruned > 0 {
        tracing::info!(count = pruned, "db export snapshots pruned");
    }
    let path = directory.join(format!(
        "{EXPORT_SNAPSHOT_PREFIX}{}{EXPORT_SNAPSHOT_SUFFIX}",
        crate::coord_core::ids::render_v4(entropy()?)
    ));
    let snapshot = create_sqlite_snapshot(database, &path)
        .await
        .map_err(ExportError::Snapshot)?;
    tracing::info!(bytes = snapshot.size, "db export snapshot ready");
    Ok(ExportSnapshot {
        path,
        size: snapshot.size,
    })
}

/// Remove this copy once the download window closes.
///
/// Detached and unref'd by construction: a tokio task does not keep the process
/// alive, which is the whole of what v2's `.unref()` bought.
pub fn schedule_reclaim(path: PathBuf) {
    tokio::spawn(async move {
        tokio::time::sleep(EXPORT_SNAPSHOT_TTL).await;
        if let Err(reason) = crate::maintenance::remove_file_if_present(&path).await {
            tracing::warn!(path = %path.display(), %reason, "db export snapshot reclaim failed");
        }
    });
}

/// Remove export copies at or past `older_than`, then every survivor beyond the
/// `keep_newest` most recent. How many were removed.
///
/// A file that vanished between the read and the removal is success, not a
/// reason to abandon the rest of the sweep: another export's own timer unlinks
/// concurrently with this pass, and that is the normal case, not a fault.
pub async fn sweep_export_snapshots(
    directory: &Path,
    older_than: Duration,
    keep_newest: usize,
) -> usize {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return 0;
    };
    let mut survivors: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut pruned = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(EXPORT_SNAPSHOT_PREFIX) || !name.ends_with(EXPORT_SNAPSHOT_SUFFIX) {
            continue;
        }
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        if older_than.is_zero() || is_older_than(modified, older_than) {
            let _ = tokio::fs::remove_file(entry.path()).await;
            // Counted here, not only at the surplus: a boot sweep removes
            // everything by AGE and would otherwise report zero while emptying
            // the directory, and the number that reaches the log is the only
            // evidence an operator has that it ran.
            pruned += 1;
            continue;
        }
        survivors.push((modified, entry.path()));
    }
    survivors.sort_by_key(|(modified, _)| *modified);
    let excess = survivors.len().saturating_sub(keep_newest);
    for (_, path) in survivors.iter().take(excess) {
        let _ = tokio::fs::remove_file(path).await;
    }
    pruned + excess
}

fn is_older_than(modified: std::time::SystemTime, bound: Duration) -> bool {
    std::time::SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= bound)
}

fn entropy() -> Result<[u8; 16], ExportError> {
    crate::coord_core::ids::draw::<16>().map_err(ExportError::Entropy)
}

/// Why an export could not be prepared.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The database path has no parent directory to write a copy beside.
    #[error("the coordinator database path has no parent directory")]
    NoDirectory,
    /// The database is a Postgres server, not a file to copy.
    #[error("db export needs the SQLite backend")]
    NotAFile,
    /// No entropy source, so the copy cannot be given a unique name.
    #[error("db export: no entropy source: {0}")]
    Entropy(#[source] std::io::Error),
    /// The copy itself failed.
    #[error("db export snapshot: {0}")]
    Snapshot(#[source] crate::maintenance::snapshot::SnapshotError),
}
