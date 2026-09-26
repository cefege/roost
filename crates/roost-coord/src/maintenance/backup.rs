//! The nightly database backup: one verified snapshot, gzipped into
//! `backups/`, published by rename, and pruned to a fixed keep count.
//!
//! Owned by the coordinator. `spawn_scheduled_backups` hands the loop a
//! [`CoordDb`]; a pre-migration backup is the same function with a different
//! reason, so the two cannot drift apart in how they write or prune.

use std::io::Error as IoError;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::db::CoordDb;
use crate::maintenance::audit_retention::DAY_MS;
use crate::maintenance::gzip_file::gzip_file_to_path;
use crate::maintenance::snapshot::{SnapshotError, create_sqlite_snapshot};
use crate::serve::now_ms;

/// How many archives are kept. 14 (`backup.ts:20`): two weeks of nightly
/// archives, at one file each.
pub const MAX_BACKUPS: usize = 14;

/// The archive name prefix. `coord_v2.` (`backup.ts:46`), kept as the contract
/// states it in §2.5 so the prune filter and any operator tooling that looks
/// for these files keep matching.
const ARCHIVE_PREFIX: &str = "coord_v2.";

/// The archive name suffix. `.db.gz`.
const ARCHIVE_SUFFIX: &str = ".db.gz";

/// What asked for a backup. v2's `"scheduled" | "pre-migration"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupReason {
    /// The nightly timer.
    Scheduled,
    /// Immediately before a migration runs.
    PreMigration,
}

impl BackupReason {
    /// The value that goes in the log line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::PreMigration => "pre-migration",
        }
    }
}

/// An archive that is on disk under its real name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRecord {
    /// The published archive.
    pub path: PathBuf,
    /// Bytes of database that went in.
    pub bytes_in: u64,
    /// Bytes of gzip that came out.
    pub bytes_out: u64,
}

/// Why a backup could not be published.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// A file operation failed. No archive was published.
    #[error("backup i/o: {0}")]
    Io(#[from] IoError),
    /// The snapshot could not be taken. No archive was published.
    #[error("backup snapshot: {0}")]
    Snapshot(#[from] SnapshotError),
}

/// The directory archives live in: a `backups` directory beside the database.
#[must_use]
pub fn backups_dir(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("backups")
}

/// Take a backup now, named for the current wall clock.
pub async fn run_backup(
    database: &CoordDb,
    reason: BackupReason,
) -> Result<BackupRecord, BackupError> {
    run_backup_at(database, reason, now_ms()).await
}

/// Take a backup whose archive name is derived from `now_ms`.
///
/// The clock is a parameter so a test can say which archive it expects, the
/// same reason v2's sweep takes an injectable `now` (`audit-retention.ts:52`).
pub async fn run_backup_at(
    database: &CoordDb,
    reason: BackupReason,
    now_ms: i64,
) -> Result<BackupRecord, BackupError> {
    let directory = backups_dir(database.path());
    let tag = backup_tag(now_ms);
    let archive_path = directory.join(format!("{ARCHIVE_PREFIX}{tag}{ARCHIVE_SUFFIX}"));
    let snapshot_path = directory.join(format!(".{ARCHIVE_PREFIX}{tag}.snapshot.db"));
    let staging_path = directory.join(format!(".{ARCHIVE_PREFIX}{tag}{ARCHIVE_SUFFIX}.tmp"));

    prepare_backup_dir(&directory).await?;

    let published = publish_archive(
        database,
        &snapshot_path,
        &staging_path,
        &archive_path,
        reason,
    )
    .await;
    if let Err(error) = &published {
        tracing::error!(
            path = %archive_path.display(),
            reason = reason.as_str(),
            error = %error,
            "backup failed"
        );
        // The staging file never earns its real name, so removing it is the
        // whole cleanup. The real name is NOT touched: it holds some earlier
        // archive, and v2's catch deletes that too (`backup.ts:68-69`), which
        // is only sound there because its rename is the last statement. Here
        // nothing can fail after the rename at all, so deleting a published
        // archive would only ever destroy a backup that is already good.
        let _ = super::remove_file_if_present(&staging_path).await;
    }
    let record = published?;

    // Prune only after a successful write, and never let it fail the backup.
    prune_backups(&directory).await;
    Ok(record)
}

async fn prepare_backup_dir(directory: &Path) -> Result<(), IoError> {
    tokio::fs::create_dir_all(directory).await?;
    super::set_owner_only_dir(directory).await
}

async fn publish_archive(
    database: &CoordDb,
    snapshot_path: &Path,
    staging_path: &Path,
    archive_path: &Path,
    reason: BackupReason,
) -> Result<BackupRecord, BackupError> {
    let snapshotted = create_sqlite_snapshot(database, snapshot_path).await;
    let published = match snapshotted {
        Ok(_) => stage_and_rename(snapshot_path, staging_path, archive_path, reason).await,
        Err(error) => Err(BackupError::Snapshot(error)),
    };
    // The uncompressed intermediate never outlives the call (`backup.ts:70-72`).
    let _ = super::remove_file_if_present(snapshot_path).await;
    published
}

async fn stage_and_rename(
    snapshot_path: &Path,
    staging_path: &Path,
    archive_path: &Path,
    reason: BackupReason,
) -> Result<BackupRecord, BackupError> {
    let compressed = gzip_file_to_path(snapshot_path, staging_path).await?;
    super::set_owner_only_file(staging_path).await?;
    // The atomic publish. A reader of the backups directory sees either the
    // previous set of names or the new one, never a half-written archive under
    // its real name.
    tokio::fs::rename(staging_path, archive_path).await?;
    tracing::info!(
        path = %archive_path.display(),
        reason = reason.as_str(),
        bytes_in = compressed.bytes_in,
        bytes_out = compressed.bytes_out,
        "backup written"
    );
    Ok(BackupRecord {
        path: archive_path.to_path_buf(),
        bytes_in: compressed.bytes_in,
        bytes_out: compressed.bytes_out,
    })
}

/// Delete everything past [`MAX_BACKUPS`], oldest name first.
///
/// A prune failure is logged and skipped. A full disk, a permission change or
/// an unremovable file must not turn an archive that was written and verified
/// into a reported backup failure (`backup.ts:74-86`).
async fn prune_backups(directory: &Path) {
    let archives = list_archives(directory).await;
    let excess = archives.len().saturating_sub(MAX_BACKUPS);
    for archive in archives.into_iter().take(excess) {
        match tokio::fs::remove_file(&archive).await {
            Ok(()) => tracing::info!(path = %archive.display(), "backup pruned"),
            Err(error) => {
                tracing::warn!(path = %archive.display(), error = %error, "backup prune failed");
            }
        }
    }
}

/// Every archive in `directory`, oldest name first.
///
/// Not filtered to regular files, because a name that is not a file is a prune
/// that will fail, and a prune that will fail has to be survivable.
pub async fn list_archives(directory: &Path) -> Vec<PathBuf> {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return Vec::new();
    };
    let mut archives: Vec<(String, PathBuf)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(ARCHIVE_PREFIX) && name.ends_with(ARCHIVE_SUFFIX) {
            archives.push((name, entry.path()));
        }
    }
    archives.sort_by(|left, right| left.0.cmp(&right.0));
    archives.into_iter().map(|(_, path)| path).collect()
}

/// Whether the newest archive is older than a day, so the first scheduled run
/// fires immediately rather than a day after boot (`backup.ts:99-103`).
async fn backup_is_stale(directory: &Path) -> bool {
    let archives = list_archives(directory).await;
    let Some(newest) = archives.last() else {
        return true;
    };
    match tokio::fs::metadata(newest).await {
        Ok(metadata) => match metadata.modified() {
            Ok(modified) => match modified.duration_since(std::time::SystemTime::now()) {
                Ok(since_now) => u64::try_from(since_now.as_millis()).unwrap_or(0) > DAY_MS_U64,
                Err(_) => false,
            },
            Err(_) => true,
        },
        Err(_) => true,
    }
}

const DAY_MS_U64: u64 = DAY_MS as u64;

/// The archive tag for a wall-clock instant: `YYYY-MM-DDTHH-MM-SS-mmm`, UTC.
///
/// v2 built it from `Date.toISOString()` by replacing `:` and `.` with `-` and
/// dropping the `Z` (`backup.ts:46`), so the name is safe on every filesystem
/// and sorts chronologically as plain text -- which is exactly what the prune
/// orders by.
#[must_use]
pub fn backup_tag(now_ms: i64) -> String {
    let total_seconds = now_ms.div_euclid(1000);
    let millis = now_ms.rem_euclid(1000);
    let (year, month, day) = civil_from_days(total_seconds.div_euclid(86_400));
    let second_of_day = total_seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}-{millis:03}",
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60,
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian civil date.
///
/// Howard Hinnant's `civil_from_days`, so the tag needs no calendar dependency
/// and no leap-year special case to get wrong.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Run the backup loop: one run if the newest archive is stale, then one a day.
///
/// The returned task is detached. A tokio task is dropped when the runtime
/// shuts down, which is the effect v2's `setInterval(…).unref()` was buying.
pub fn spawn_scheduled_backups(database: CoordDb) {
    if !database.path().exists() {
        tracing::warn!(db_path = %database.path().display(), "backup skipped: no database");
        return;
    }
    let directory = backups_dir(database.path());
    tokio::spawn(async move {
        if backup_is_stale(&directory).await {
            run_and_log(&database, BackupReason::Scheduled).await;
        }
        loop {
            tokio::time::sleep(Duration::from_millis(DAY_MS_U64)).await;
            run_and_log(&database, BackupReason::Scheduled).await;
        }
    });
}

async fn run_and_log(database: &CoordDb, reason: BackupReason) {
    // `run_backup` already logged the actionable error; scheduled work has no
    // caller to return one to (`backup.ts:101-105`).
    let _ = run_backup(database, reason).await;
}
