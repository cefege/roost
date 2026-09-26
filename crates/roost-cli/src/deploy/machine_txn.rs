//! The machine transaction: one OS-backed SQLite write transaction held for
//! the whole of a machine mutation, so a deploy, a keeper refresh and a second
//! deploy cannot interleave on one machine. Called by the target-side apply
//! driver and by the target-side transaction command, both of which run on the
//! machine being changed; the deploying side never opens this file.
//!
//! This is a lock, not a journal. The durable record of what a deploy is doing
//! is `services::deploy_journal.rs`, and this deliberately knows nothing about
//! it beyond the path it names: a transaction that is released must leave
//! nothing behind, and a transaction that is killed must leave nothing to clean
//! up. SQLite's kernel file lock gives both — it is atomic, it is released by
//! the kernel when the process dies, and it cannot be confused by a reused pid
//! the way a pid file can.
//!
//! Every failure to take the lock is a refusal and never a wait. A deploy that
//! queued behind another one would be a deploy whose rollback point was
//! computed before the mutation it rolls back.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The file a machine's transaction lock lives in, inside the service directory
/// that already holds this install's deploy state.
pub const MACHINE_TRANSACTION_FILE: &str = "machine-transaction.sqlite";

/// What a machine transaction is for. Recorded so the holder a later operator
/// finds can be told what it was doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransactionKind {
    /// A definition swap: `roost deploy`.
    Deploy,
    /// A keeper shutdown: `roost keeper-refresh`.
    KeeperRefresh,
}

impl TransactionKind {
    /// The value written into the record, and the value the command line takes.
    pub const fn as_str(self) -> &'static str {
        match self {
            TransactionKind::Deploy => "deploy",
            TransactionKind::KeeperRefresh => "keeper-refresh",
        }
    }

    /// Parse a kind named on a command line. An unrecognised word is refused
    /// rather than defaulted: a lock taken for the wrong reason is a lock the
    /// next operator cannot interpret.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "deploy" => Ok(TransactionKind::Deploy),
            "keeper-refresh" => Ok(TransactionKind::KeeperRefresh),
            other => Err(format!(
                "unknown machine transaction kind {other:?}; expected deploy or keeper-refresh"
            )),
        }
    }
}

/// The row a held transaction leaves, so a machine that is already busy can say
/// what is busy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTransactionRecord {
    /// What the holder is doing.
    pub kind: TransactionKind,
    /// The journal whose recovery this transaction is serialising against.
    pub journal_path: String,
    /// The holding process, for an operator reading the database.
    pub owner_pid: i32,
    /// The holder's process epoch. A pid alone cannot distinguish this run from
    /// a later one that reused the number.
    pub process_epoch: String,
    /// When it was taken, in milliseconds since the epoch.
    pub acquired_at_ms: i64,
}

/// Every way a transaction can fail to be taken.
#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("another machine transaction is already active{}", .holder.as_deref().map_or_else(String::new, |record| format!(": {record}")))]
    Busy {
        /// The record the previous holder left, as a sentence for the message.
        holder: Option<String>,
    },
    #[error("the machine transaction database at {path} is unusable: {cause}")]
    Unusable {
        /// The lock file.
        path: PathBuf,
        /// What went wrong with it.
        cause: String,
    },
    #[error("the machine transaction record at {path} is unreadable: {cause}")]
    UnreadableRecord {
        /// The lock file.
        path: PathBuf,
        /// Why the row did not decode.
        cause: String,
    },
}

/// The lock file for a service directory.
pub fn lock_path(service_dir: &Path) -> PathBuf {
    service_dir.join(MACHINE_TRANSACTION_FILE)
}

/// A held machine transaction. Dropping it rolls back, which releases the
/// kernel lock; [`MachineTransaction::release`] is the explicit form and is
/// what the target-side command calls when its work is done.
#[derive(Debug)]
pub struct MachineTransaction {
    pool: SqlitePool,
    path: PathBuf,
    record: MachineTransactionRecord,
    released: bool,
}

impl MachineTransaction {
    /// Take the machine transaction for `kind`, naming the journal whose
    /// recovery it serialises.
    pub async fn acquire(
        lock_file: &Path,
        kind: TransactionKind,
        journal_path: &Path,
        now_ms: i64,
    ) -> Result<Self, TransactionError> {
        if let Some(parent) = lock_file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| TransactionError::Unusable {
                path: lock_file.to_path_buf(),
                cause: format!("{}: {error}", parent.display()),
            })?;
        }
        let pool = open(lock_file).await?;
        let record = MachineTransactionRecord {
            kind,
            journal_path: journal_path.display().to_string(),
            owner_pid: std::process::id() as i32,
            process_epoch: roost_worker::runtime::boot::new_process_epoch(),
            acquired_at_ms: now_ms,
        };
        let encoded = serde_json::to_string(&record)
            .map_err(|error| unusable(lock_file, sqlx::Error::Protocol(error.to_string())))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS active_machine_transaction (\
               singleton INTEGER PRIMARY KEY CHECK (singleton = 1),\
               record_json TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .map_err(|error| unusable(lock_file, error))?;
        // `busy_timeout = 0` is what makes a contended take fail instead of
        // wait: the second deploy is refused while the first still holds it,
        // which is the only ordering that keeps a rollback point meaningful.
        sqlx::query("BEGIN EXCLUSIVE")
            .execute(&pool)
            .await
            .map_err(|error| {
                if is_busy(&error) {
                    TransactionError::Busy { holder: None }
                } else {
                    unusable(lock_file, error)
                }
            })?;
        let held: Option<String> = sqlx::query_scalar(
            "SELECT record_json FROM active_machine_transaction WHERE singleton = 1",
        )
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();
        if let Some(existing) = held {
            let _ = sqlx::query("ROLLBACK").execute(&pool).await;
            return Err(TransactionError::Busy {
                holder: decode_record(lock_file, &existing)
                    .ok()
                    .map(|record| format!("({record:?})")),
            });
        }
        if let Err(error) = sqlx::query(
            "INSERT INTO active_machine_transaction (singleton, record_json) VALUES (1, ?)",
        )
        .bind(&encoded)
        .execute(&pool)
        .await
        {
            let _ = sqlx::query("ROLLBACK").execute(&pool).await;
            return Err(unusable(lock_file, error));
        }
        Ok(Self {
            pool,
            path: lock_file.to_path_buf(),
            record,
            released: false,
        })
    }

    /// The lock file this transaction holds.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The record this holder left.
    pub fn record(&self) -> &MachineTransactionRecord {
        &self.record
    }

    /// Give the machine back: remove the row and commit, which is what releases
    /// the kernel lock.
    pub async fn release(mut self) -> Result<(), TransactionError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        sqlx::query("DELETE FROM active_machine_transaction WHERE singleton = 1")
            .execute(&self.pool)
            .await
            .map_err(|error| unusable(&self.path, error))?;
        sqlx::query("COMMIT")
            .execute(&self.pool)
            .await
            .map_err(|error| unusable(&self.path, error))?;
        self.pool.close().await;
        Ok(())
    }
}

/// The transaction a machine is under right now, or `None` when it is idle.
///
/// Read with the same exclusive transaction the holder uses, so a caller can
/// never see a half-written row: a machine that is mid-take is mid-take.
pub async fn active_transaction(
    lock_file: &Path,
) -> Result<Option<MachineTransactionRecord>, TransactionError> {
    if !lock_file.exists() {
        return Ok(None);
    }
    let pool = open(lock_file).await?;
    let held: Option<String> = sqlx::query_scalar(
        "SELECT record_json FROM active_machine_transaction WHERE singleton = 1",
    )
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();
    pool.close().await;
    match held {
        None => Ok(None),
        Some(encoded) => decode_record(lock_file, &encoded).map(Some),
    }
}

async fn open(lock_file: &Path) -> Result<SqlitePool, TransactionError> {
    let options = SqliteConnectOptions::new()
        .filename(lock_file)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::ZERO)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| unusable(lock_file, error))
}

fn decode_record(
    lock_file: &Path,
    encoded: &str,
) -> Result<MachineTransactionRecord, TransactionError> {
    serde_json::from_str(encoded).map_err(|error| TransactionError::UnreadableRecord {
        path: lock_file.to_path_buf(),
        cause: error.to_string(),
    })
}

fn unusable(path: &Path, error: sqlx::Error) -> TransactionError {
    TransactionError::Unusable {
        path: path.to_path_buf(),
        cause: error.to_string(),
    }
}

fn is_busy(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database) if database.code().as_deref() == Some("SQLITE_BUSY")
    )
}
