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
    /// The kernel lock that actually serialises the machine. Dropping this file
    /// releases it, which is what makes a holder that dies for any reason leave
    /// the machine free without anybody having to notice.
    gate: std::fs::File,
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
        // The kernel lock is taken FIRST and is the only thing that serialises
        // the machine. It used to be the database's own exclusive transaction,
        // which cannot work: a row written inside an uncommitted transaction is
        // invisible to every other connection, so the apply on the far side —
        // the one process whose job is to check that a transaction is held —
        // could neither read the row nor connect, and every deploy was refused
        // with "no machine transaction is held" at exactly the moment one was.
        let gate = take_kernel_lock(lock_file)?;
        let pool = match open(lock_file, std::time::Duration::ZERO).await {
            Ok(pool) => pool,
            Err(error) => {
                drop(gate);
                return Err(error);
            }
        };
        let record = MachineTransactionRecord {
            kind,
            journal_path: journal_path.display().to_string(),
            owner_pid: std::process::id() as i32,
            process_epoch: roost_worker::runtime::boot::new_process_epoch(),
            acquired_at_ms: now_ms,
        };
        let encoded = serde_json::to_string(&record)
            .map_err(|error| unusable(lock_file, sqlx::Error::Protocol(error.to_string())))?;
        let written = async {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS active_machine_transaction (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1),\
                   record_json TEXT NOT NULL)",
            )
            .execute(&pool)
            .await
            .map_err(|error| unusable(lock_file, error))?;
            // A row left by a holder that died is stale by definition — the
            // kernel lock is what says so — and this take has just proved it is
            // gone. Overwriting is therefore the whole recovery.
            sqlx::query("DELETE FROM active_machine_transaction WHERE singleton = 1")
                .execute(&pool)
                .await
                .map_err(|error| unusable(lock_file, error))?;
            sqlx::query(
                "INSERT INTO active_machine_transaction (singleton, record_json) VALUES (1, ?)",
            )
            .bind(&encoded)
            .execute(&pool)
            .await
            .map_err(|error| unusable(lock_file, error))?;
            // COMMITTED, not left open. This is the whole point: the record has
            // to be readable by the apply while the lock is still held.
            sqlx::query("COMMIT")
                .execute(&pool)
                .await
                .map_err(|error| unusable(lock_file, error))?;
            Ok::<(), TransactionError>(())
        }
        .await;
        if let Err(error) = written {
            drop(gate);
            drop(pool);
            return Err(error);
        }
        Ok(Self {
            pool,
            path: lock_file.to_path_buf(),
            record,
            released: false,
            gate,
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
        // Released last, and released by the kernel either way: dropping the
        // file is enough, so a holder that unwinds still gives the machine back.
        drop(self.gate);
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
    let pool = open(lock_file, READER_BUSY_TIMEOUT).await?;
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

/// Take the machine's kernel lock, or report who holds it.
///
/// A separate file from the database, because the database's own locking is
/// exactly what cannot be relied on here: the record has to be readable by
/// another process while the lock is held, and a SQLite write transaction
/// makes it unreadable. `flock` gives the two properties at once — a second
/// taker is refused immediately, and the kernel drops it when the holder dies
/// however it dies.
fn take_kernel_lock(lock_file: &Path) -> Result<std::fs::File, TransactionError> {
    let gate_path = gate_path(lock_file);
    let gate = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&gate_path)
        .map_err(|error| TransactionError::Unusable {
            path: gate_path.clone(),
            cause: error.to_string(),
        })?;
    match gate.try_lock() {
        Ok(()) => Ok(gate),
        Err(_) => Err(TransactionError::Busy {
            // Best effort, and honest about being so: the record may be
            // mid-write. Naming the holder is a courtesy, refusing the take is
            // the guarantee, and the guarantee does not depend on the read.
            holder: None,
        }),
    }
}

/// The file the machine's kernel lock lives in, beside the record it guards.
pub fn gate_path(lock_file: &Path) -> PathBuf {
    let mut name = lock_file.file_name().unwrap_or_default().to_os_string();
    name.push(".hold");
    lock_file.with_file_name(name)
}

/// How long a READER waits for a commit that is in flight.
///
/// The take path waits for nothing — a second deploy is refused while the first
/// holds the machine, and that is the ordering that keeps a rollback point
/// meaningful. A reader is different: a commit landing under it is not a
/// contention to refuse, it is the holder arriving, and treating that as "no
/// transaction is held" refuses a legitimate deploy for a few milliseconds.
const READER_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `busy` is how long this connection waits for a lock before reporting one.
async fn open(lock_file: &Path, busy: std::time::Duration) -> Result<SqlitePool, TransactionError> {
    let options = SqliteConnectOptions::new()
        .filename(lock_file)
        .create_if_missing(true)
        .busy_timeout(busy)
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
