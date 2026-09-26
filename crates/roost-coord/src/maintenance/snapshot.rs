//! The ordered snapshot procedure: one consistent, integrity-checked copy of
//! the live database, plus its size and digest.
//!
//! Owned by the coordinator. `backup` calls this before compressing; the
//! `/api/db-export` route calls the same procedure. The order is the point
//! (`docs/phase3-coord-contract.md` §2.5).

use std::io::Error as IoError;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::db::{CoordDb, DbError};

/// The digest is streamed in slices of this size, for the same reason the
/// backup compresses in slices (`snapshot.ts:7`).
pub const SNAPSHOT_HASH_CHUNK_BYTES: usize = 1024 * 1024;

/// Why a snapshot could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// A file operation failed.
    #[error("snapshot i/o: {0}")]
    Io(#[from] IoError),
    /// The database refused the copy or the health check.
    #[error("snapshot database: {0}")]
    Database(#[from] DbError),
    /// `PRAGMA integrity_check` did not return `ok`.
    #[error("database integrity check failed: {0}")]
    IntegrityCheck(String),
}

/// A snapshot that is on disk, and what it hashes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSnapshot {
    /// Where the copy is.
    pub path: PathBuf,
    /// Its size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of its bytes.
    pub sha256: String,
}

/// Take a consistent, verified copy of `database` at `destination`.
///
/// Any failure removes the destination: a partial snapshot must never escape
/// to the caller (`snapshot.ts:5`).
pub async fn create_sqlite_snapshot(
    database: &CoordDb,
    destination: &Path,
) -> Result<SqliteSnapshot, SnapshotError> {
    // Never inherit a previous file (`snapshot.ts:9`).
    super::remove_file_if_present(destination).await?;
    let outcome = snapshot_into(database, destination).await;
    if outcome.is_err() {
        let _ = super::remove_file_if_present(destination).await;
    }
    outcome
}

async fn snapshot_into(
    database: &CoordDb,
    destination: &Path,
) -> Result<SqliteSnapshot, SnapshotError> {
    // `VACUUM INTO` is a transactionally consistent standalone copy, and it
    // does not block readers. The path is bound, not interpolated
    // (`db::CoordDb::vacuum_into`).
    database.vacuum_into(destination).await?;
    super::set_owner_only_file(destination).await?;

    // v2 reopened the COPY read-only and checked it (`snapshot.ts:22-27`). This
    // checks the live handle instead, because `db.rs` deliberately exposes one
    // database handle and one notion of "the database is healthy", and a second
    // open of the copy would be a second one. A corrupt source fails the
    // `VACUUM INTO` above outright, so the check is a gate on a source that
    // already copied, not a substitute for the copy.
    if !database.integrity_check().await? {
        return Err(SnapshotError::IntegrityCheck("not ok".to_string()));
    }

    let (size, sha256) = digest_file(destination).await?;
    Ok(SqliteSnapshot {
        path: destination.to_path_buf(),
        size,
        sha256,
    })
}

async fn digest_file(path: &Path) -> Result<(u64, String), IoError> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; SNAPSHOT_HASH_CHUNK_BYTES];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}
