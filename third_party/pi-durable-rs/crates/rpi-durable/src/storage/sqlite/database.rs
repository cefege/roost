//! Asynchronous SQLite facade (port of `src/storage/sqlite/database.ts` and
//! `node.ts` @ `abe508e1b`).
//!
//! Selected stack (FR-C R4): `rusqlite` + `bundled` + a dedicated blocking
//! worker thread. All operations are serialized in call order; a transaction
//! owns the connection until its callback settles. Upstream's
//! `SerialOperationQueue` maps to a FIFO Tokio mutex plus a worker-thread
//! channel.

use rusqlite::Connection;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::{StorageError, StorageResult};

/// Values supported by the portable SQLite storage core.
#[derive(Debug, Clone, PartialEq)]
pub enum SqliteValue {
    /// SQL NULL.
    Null,
    /// 64-bit integer.
    Integer(i64),
    /// 64-bit float.
    Real(f64),
    /// UTF-8 text.
    Text(String),
    /// Byte blob.
    Blob(Vec<u8>),
}

impl SqliteValue {
    fn into_rusqlite(self) -> rusqlite::types::Value {
        match self {
            SqliteValue::Null => rusqlite::types::Value::Null,
            SqliteValue::Integer(value) => rusqlite::types::Value::Integer(value),
            SqliteValue::Real(value) => rusqlite::types::Value::Real(value),
            SqliteValue::Text(value) => rusqlite::types::Value::Text(value),
            SqliteValue::Blob(value) => rusqlite::types::Value::Blob(value),
        }
    }
}

impl From<i64> for SqliteValue {
    fn from(value: i64) -> Self {
        SqliteValue::Integer(value)
    }
}
impl From<u64> for SqliteValue {
    fn from(value: u64) -> Self {
        SqliteValue::Integer(value as i64)
    }
}
impl From<usize> for SqliteValue {
    fn from(value: usize) -> Self {
        SqliteValue::Integer(value as i64)
    }
}
impl From<bool> for SqliteValue {
    fn from(value: bool) -> Self {
        SqliteValue::Integer(i64::from(value))
    }
}
impl From<String> for SqliteValue {
    fn from(value: String) -> Self {
        SqliteValue::Text(value)
    }
}
impl From<&str> for SqliteValue {
    fn from(value: &str) -> Self {
        SqliteValue::Text(value.to_string())
    }
}
impl<T: Into<SqliteValue>> From<Option<T>> for SqliteValue {
    fn from(value: Option<T>) -> Self {
        match value {
            None => SqliteValue::Null,
            Some(value) => value.into(),
        }
    }
}
impl<T: Into<SqliteValue> + Clone> From<&T> for SqliteValue {
    fn from(value: &T) -> Self {
        value.clone().into()
    }
}

/// Read-only borrow of one bound SQL value.
pub type SqliteValueRef = SqliteValue;

fn rusqlite_params(params: &[SqliteValue]) -> Vec<rusqlite::types::Value> {
    params
        .iter()
        .cloned()
        .map(SqliteValue::into_rusqlite)
        .collect()
}

/// Executes one SQL statement against a connection.
pub(crate) fn query_json<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: &[SqliteValue],
) -> StorageResult<Option<T>> {
    let mut statement = conn.prepare_cached(sql)?;
    let bindings = rusqlite_params(params);
    let mut rows = statement.query(rusqlite::params_from_iter(bindings.iter()))?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let text: String = row.get(0)?;
    Ok(Some(serde_json::from_str(&text)?))
}

pub(crate) fn query_all_json<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: &[SqliteValue],
) -> StorageResult<Vec<T>> {
    let mut statement = conn.prepare_cached(sql)?;
    let bindings = rusqlite_params(params);
    let mut rows = statement.query(rusqlite::params_from_iter(bindings.iter()))?;
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        let text: String = row.get(0)?;
        values.push(serde_json::from_str(&text)?);
    }
    Ok(values)
}

pub(crate) fn query_i64(
    conn: &Connection,
    sql: &str,
    params: &[SqliteValue],
) -> StorageResult<Option<i64>> {
    let mut statement = conn.prepare_cached(sql)?;
    let bindings = rusqlite_params(params);
    let mut rows = statement.query(rusqlite::params_from_iter(bindings.iter()))?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(row.get(0)?))
}

pub(crate) fn query_text(
    conn: &Connection,
    sql: &str,
    params: &[SqliteValue],
) -> StorageResult<Option<String>> {
    let mut statement = conn.prepare_cached(sql)?;
    let bindings = rusqlite_params(params);
    let mut rows = statement.query(rusqlite::params_from_iter(bindings.iter()))?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(row.get(0)?))
}

struct Worker {
    sender: std::sync::mpsc::Sender<Job>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

enum Job {
    Run(Box<dyn FnOnce(&Connection) + Send>),
    Close(tokio::sync::oneshot::Sender<()>),
}

use std::sync::Mutex;

impl Worker {
    fn send(&self, job: Job) -> StorageResult<()> {
        self.sender
            .send(job)
            .map_err(|_| StorageError::Closed("SQLite database"))
    }

    fn shutdown(&self) {
        let (ack, wait) = tokio::sync::oneshot::channel();
        if self.sender.send(Job::Close(ack)).is_ok() {
            let _ = wait.blocking_recv();
        }
        if let Ok(mut handle) = self.handle.lock()
            && let Some(handle) = handle.take()
        {
            let _ = handle.join();
        }
    }

    fn run<T: Send + 'static>(
        &self,
        op: impl FnOnce(&Connection) -> StorageResult<T> + Send + 'static,
    ) -> StorageResult<T> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.send(Job::Run(Box::new(move |conn| {
            let _ = tx.send(op(conn));
        })))?;
        rx.recv()
            .map_err(|_| StorageError::Closed("SQLite database"))?
    }

    /// Queue a ROLLBACK without waiting for it. Used by [`RollbackGuard`] when
    /// a transaction future is dropped before COMMIT/ROLLBACK. The worker is
    /// FIFO, so the rollback runs after every statement the callback already
    /// queued and before the next operation can acquire the access mutex.
    fn rollback_now(&self, broken: Arc<AtomicBool>) {
        let broken_for_job = Arc::clone(&broken);
        let sent = self.sender.send(Job::Run(Box::new(move |conn| {
            let failed = conn.execute_batch("ROLLBACK").is_err();
            // "no transaction is active" after a callback escaped with its
            // own COMMIT is benign: the connection is still usable.
            if failed && !conn.is_autocommit() {
                broken_for_job.store(true, Ordering::SeqCst);
            }
        })));
        if sent.is_err() {
            broken.store(true, Ordering::SeqCst);
        }
    }
}

/// Reset an atomic flag when dropped unless disarmed. Close paths use this so
/// a cancelled or failed await leaves the resource retryable instead of
/// permanently marked done.
struct ResetOnDrop<'a> {
    flag: &'a AtomicBool,
    armed: bool,
}

impl<'a> ResetOnDrop<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        Self { flag, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ResetOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(false, Ordering::SeqCst);
        }
    }
}

/// RAII rollback for `SqliteDatabase::transaction`.
///
/// Declared after the access-mutex guard so its `Drop` runs first: the
/// ROLLBACK job is queued while the access mutex is still held, therefore no
/// later operation can observe/continue the open transaction.
struct RollbackGuard {
    worker: Arc<Worker>,
    active: Arc<AtomicBool>,
    broken: Arc<AtomicBool>,
    settled: bool,
}

impl RollbackGuard {
    fn new(worker: Arc<Worker>, active: Arc<AtomicBool>, broken: Arc<AtomicBool>) -> Self {
        Self {
            worker,
            active,
            broken,
            settled: false,
        }
    }

    fn disarm(&mut self) {
        self.settled = true;
    }
}

impl Drop for RollbackGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.active.store(false, Ordering::SeqCst);
        self.worker.rollback_now(Arc::clone(&self.broken));
    }
}

/// Connection settings for a durable SQLite database.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteDatabaseOptions {
    /// SQLite WAL auto-checkpoint threshold; `Some(0)` disables it. Default 1000 pages.
    pub wal_auto_checkpoint_pages: Option<i64>,
    /// Time SQLite waits for a competing file lock. Default 5000 ms.
    pub busy_timeout_ms: Option<u64>,
}

/// Default SQLite WAL auto-checkpoint threshold (`node.ts:16`).
pub const DEFAULT_WAL_AUTO_CHECKPOINT_PAGES: i64 = 1_000;
/// Default busy timeout (`node.ts:17`).
pub const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;

tokio::task_local! {
    static IN_TRANSACTION: std::cell::Cell<bool>;
}

fn in_transaction() -> bool {
    IN_TRANSACTION.try_with(|flag| flag.get()).unwrap_or(false)
}

/// Asynchronous SQL executor shared by a database and its transaction handles.
pub struct SqliteExecutor {
    worker: Arc<Worker>,
}

impl SqliteExecutor {
    /// Run SQL text without bindings; may contain several statements.
    pub async fn exec(&self, sql: &str) -> StorageResult<()> {
        self.with_connection({
            let sql = sql.to_string();
            move |conn| {
                conn.execute_batch(&sql)?;
                Ok(())
            }
        })
        .await
    }

    /// Whether the connection is outside an explicit transaction.
    pub async fn is_autocommit(&self) -> StorageResult<bool> {
        self.with_connection(|conn| Ok(conn.is_autocommit())).await
    }

    /// Execute one statement with positional bindings.
    pub async fn run(&self, sql: &str, params: Vec<SqliteValue>) -> StorageResult<()> {
        self.with_connection({
            let sql = sql.to_string();
            move |conn| {
                let mut statement = conn.prepare_cached(&sql)?;
                let bindings = rusqlite_params(&params);
                statement.execute(rusqlite::params_from_iter(bindings.iter()))?;
                Ok(())
            }
        })
        .await
    }

    /// Run one statement with positional bindings and read the first row as JSON.
    pub async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Option<T>> {
        self.with_connection({
            let sql = sql.to_string();
            move |conn| query_json(conn, &sql, &params)
        })
        .await
    }

    /// Run one statement and read every row's first column as JSON.
    pub async fn all<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Vec<T>> {
        self.with_connection({
            let sql = sql.to_string();
            move |conn| query_all_json(conn, &sql, &params)
        })
        .await
    }

    /// Run one statement and read the first row's first column as an integer.
    pub async fn get_i64(&self, sql: &str, params: Vec<SqliteValue>) -> StorageResult<Option<i64>> {
        self.with_connection({
            let sql = sql.to_string();
            move |conn| query_i64(conn, &sql, &params)
        })
        .await
    }

    /// Run one statement and read the first row's first column as text.
    pub async fn get_text(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Option<String>> {
        self.with_connection({
            let sql = sql.to_string();
            move |conn| query_text(conn, &sql, &params)
        })
        .await
    }

    /// Escape hatch for statements that need the raw rusqlite connection.
    pub async fn with_connection<T: Send + 'static>(
        &self,
        op: impl FnOnce(&Connection) -> StorageResult<T> + Send + 'static,
    ) -> StorageResult<T> {
        let worker = Arc::clone(&self.worker);
        tokio::task::spawn_blocking(move || worker.run(op))
            .await
            .map_err(|error| StorageError::message(format!("SQLite worker failed: {error}")))?
    }
}

/// Handle of one open transaction; invalid once its callback settles.
pub struct SqliteTransaction {
    executor: SqliteExecutor,
    active: Arc<AtomicBool>,
}

impl SqliteTransaction {
    fn assert_active(&self) -> StorageResult<()> {
        if self.active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(StorageError::TransactionInactive)
        }
    }

    /// See [`SqliteExecutor::exec`].
    pub async fn exec(&self, sql: &str) -> StorageResult<()> {
        self.assert_active()?;
        self.executor.exec(sql).await
    }

    /// See [`SqliteExecutor::run`].
    pub async fn run(&self, sql: &str, params: Vec<SqliteValue>) -> StorageResult<()> {
        self.assert_active()?;
        self.executor.run(sql, params).await
    }

    /// See [`SqliteExecutor::get`].
    pub async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Option<T>> {
        self.assert_active()?;
        self.executor.get(sql, params).await
    }

    /// See [`SqliteExecutor::all`].
    pub async fn all<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Vec<T>> {
        self.assert_active()?;
        self.executor.all(sql, params).await
    }

    /// See [`SqliteExecutor::get_i64`].
    pub async fn get_i64(&self, sql: &str, params: Vec<SqliteValue>) -> StorageResult<Option<i64>> {
        self.assert_active()?;
        self.executor.get_i64(sql, params).await
    }

    /// See [`SqliteExecutor::get_text`].
    pub async fn get_text(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Option<String>> {
        self.assert_active()?;
        self.executor.get_text(sql, params).await
    }

    /// See [`SqliteExecutor::with_connection`].
    pub async fn with_connection<T: Send + 'static>(
        &self,
        op: impl FnOnce(&Connection) -> StorageResult<T> + Send + 'static,
    ) -> StorageResult<T> {
        self.assert_active()?;
        self.executor.with_connection(op).await
    }
}

/// Minimal database facade required by [`crate::storage::sqlite::SqliteStorage`].
#[derive(Clone)]
pub struct SqliteDatabase {
    inner: Arc<DbInner>,
}

struct DbInner {
    executor: SqliteExecutor,
    access: tokio::sync::Mutex<()>,
    closed: AtomicBool,
    /// Set when a transaction could not be rolled back. The connection state
    /// is unknown, so every later operation must be rejected until reopen.
    broken: Arc<AtomicBool>,
}

impl SqliteDatabase {
    /// Open and configure a SQLite database file (or `:memory:`).
    pub async fn open(path: &str, options: SqliteDatabaseOptions) -> StorageResult<Self> {
        let checkpoint_pages = options
            .wal_auto_checkpoint_pages
            .unwrap_or(DEFAULT_WAL_AUTO_CHECKPOINT_PAGES);
        let timeout = options.busy_timeout_ms.unwrap_or(DEFAULT_BUSY_TIMEOUT_MS);
        if path != ":memory:"
            && let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let path = path.to_string();
        let (sender, receiver) = std::sync::mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<StorageResult<()>>(1);
        let handle = std::thread::Builder::new()
            .name("rpi-durable-sqlite".to_string())
            .spawn(move || {
                let connection = Connection::open(&path);
                let connection = match connection {
                    Ok(connection) => connection,
                    Err(error) => {
                        let _ = ready_tx.send(Err(StorageError::Sqlite(error)));
                        return;
                    }
                };
                let configured = (|| -> StorageResult<()> {
                    connection.busy_timeout(std::time::Duration::from_millis(timeout))?;
                    connection.execute_batch("PRAGMA journal_mode = WAL")?;
                    connection.execute_batch("PRAGMA synchronous = NORMAL")?;
                    connection.execute_batch(&format!(
                        "PRAGMA wal_autocheckpoint = {checkpoint_pages}"
                    ))?;
                    Ok(())
                })();
                if let Err(error) = configured {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
                let _ = ready_tx.send(Ok(()));
                while let Ok(job) = receiver.recv() {
                    match job {
                        Job::Run(job) => job(&connection),
                        Job::Close(ack) => {
                            let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
                            let _ = ack.send(());
                            break;
                        }
                    }
                }
            })
            .map_err(|error| {
                StorageError::message(format!("SQLite worker spawn failed: {error}"))
            })?;
        let init = ready_rx
            .recv()
            .map_err(|_| StorageError::message("SQLite worker failed to start"))?;
        init?;
        let worker = Arc::new(Worker {
            sender,
            handle: Mutex::new(Some(handle)),
        });
        Ok(Self {
            inner: Arc::new(DbInner {
                executor: SqliteExecutor { worker },
                access: tokio::sync::Mutex::new(()),
                closed: AtomicBool::new(false),
                broken: Arc::new(AtomicBool::new(false)),
            }),
        })
    }

    /// Clone the database handle (shared worker and close state).
    pub fn clone_handle(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    fn assert_open(&self) -> StorageResult<()> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(StorageError::Closed("SQLite database"));
        }
        if self.inner.broken.load(Ordering::SeqCst) {
            return Err(StorageError::Message(
                "SQLite database is poisoned after a failed transaction rollback; reopen it"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// See [`SqliteExecutor::exec`].
    pub async fn exec(&self, sql: &str) -> StorageResult<()> {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.exec(sql).await
    }

    /// See [`SqliteExecutor::run`].
    pub async fn run(&self, sql: &str, params: Vec<SqliteValue>) -> StorageResult<()> {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.run(sql, params).await
    }

    /// See [`SqliteExecutor::get`].
    pub async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Option<T>> {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.get(sql, params).await
    }

    /// See [`SqliteExecutor::all`].
    pub async fn all<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Vec<T>> {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.all(sql, params).await
    }

    /// See [`SqliteExecutor::get_i64`].
    pub async fn get_i64(&self, sql: &str, params: Vec<SqliteValue>) -> StorageResult<Option<i64>> {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.get_i64(sql, params).await
    }

    /// See [`SqliteExecutor::get_text`].
    pub async fn get_text(
        &self,
        sql: &str,
        params: Vec<SqliteValue>,
    ) -> StorageResult<Option<String>> {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.get_text(sql, params).await
    }

    /// See [`SqliteExecutor::with_connection`].
    pub async fn with_connection<T: Send + 'static>(
        &self,
        op: impl FnOnce(&Connection) -> StorageResult<T> + Send + 'static,
    ) -> StorageResult<T> {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.with_connection(op).await
    }

    /// Run `callback` inside one `BEGIN IMMEDIATE` transaction. The handle is
    /// invalid after the callback settles; unrelated operations and other
    /// transactions queue until this one finishes.
    pub async fn transaction<T, F, Fut>(&self, callback: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(SqliteTransaction) -> Fut,
        Fut: std::future::Future<Output = StorageResult<T>>,
    {
        self.assert_open()?;
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        let _guard = self.inner.access.lock().await;
        self.assert_open()?;
        self.inner.executor.exec("BEGIN IMMEDIATE").await?;
        let active = Arc::new(AtomicBool::new(true));
        let transaction = SqliteTransaction {
            executor: SqliteExecutor {
                worker: Arc::clone(&self.inner.executor.worker),
            },
            active: Arc::clone(&active),
        };
        // Declared after `_guard`: on cancellation this guard drops before the
        // access mutex, so ROLLBACK is queued before another operation runs.
        let mut rollback = RollbackGuard::new(
            Arc::clone(&self.inner.executor.worker),
            Arc::clone(&active),
            Arc::clone(&self.inner.broken),
        );
        let outcome = IN_TRANSACTION
            .scope(std::cell::Cell::new(true), async move {
                callback(transaction).await
            })
            .await;
        active.store(false, Ordering::SeqCst);
        match outcome {
            Ok(value) => {
                if let Err(commit_error) = self.inner.executor.exec("COMMIT").await {
                    return match self.inner.executor.exec("ROLLBACK").await {
                        Ok(()) => {
                            rollback.disarm();
                            Err(commit_error)
                        }
                        Err(rollback_error) => {
                            rollback.disarm();
                            if !self.inner.executor.is_autocommit().await.unwrap_or(false) {
                                self.inner.broken.store(true, Ordering::SeqCst);
                            }
                            Err(StorageError::RollbackFailed {
                                callback: Box::new(commit_error),
                                rollback: Box::new(rollback_error),
                            })
                        }
                    };
                }
                rollback.disarm();
                Ok(value)
            }
            Err(callback_error) => match self.inner.executor.exec("ROLLBACK").await {
                Ok(()) => {
                    rollback.disarm();
                    Err(callback_error)
                }
                Err(rollback_error) => {
                    rollback.disarm();
                    if !self.inner.executor.is_autocommit().await.unwrap_or(false) {
                        self.inner.broken.store(true, Ordering::SeqCst);
                    }
                    Err(StorageError::RollbackFailed {
                        callback: Box::new(callback_error),
                        rollback: Box::new(rollback_error),
                    })
                }
            },
        }
    }

    /// Release the database; repeated closes are no-ops.
    pub async fn close(&self) -> StorageResult<()> {
        if in_transaction() {
            return Err(StorageError::TransactionReentrant);
        }
        // Mark closed inside the access critical section so operations that
        // queued before close are served in FIFO order (upstream queue).
        let _guard = self.inner.access.lock().await;
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // A cancelled/failed shutdown must leave close retryable.
        let mut reset = ResetOnDrop::new(&self.inner.closed);
        let worker = Arc::clone(&self.inner.executor.worker);
        tokio::task::spawn_blocking(move || worker.shutdown())
            .await
            .map_err(|error| StorageError::message(format!("SQLite close failed: {error}")))?;
        reset.disarm();
        Ok(())
    }
}
