//! Storage error surface (port of `src/errors.ts` @ `abe508e1b` + the validation errors the
//! storage backends raise; upstream uses plain `Error` subclasses with the same
//! messages).

use thiserror::Error;

use crate::ids::{ConversationId, Id};

/// Failure modes raised by [`crate::storage::Storage`] implementations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The operation failed because a prior storage failure ended its session.
    #[error("Session failed: {0}")]
    SessionFailed(String),
    /// Storage rejected a batch before any durable effect (upstream `StorageRejected`).
    #[error("{0}")]
    Rejected(String),

    /// Generic validation failure; the message mirrors the upstream `Error`.
    #[error("{0}")]
    Message(String),

    /// Converation lookup in a fork-aware scan.
    #[error("Unknown conversation: {0}")]
    UnknownConversation(ConversationId),

    /// A cursor whose `after`/`order` fields are not a valid storage cursor.
    #[error("Invalid storage cursor")]
    InvalidCursor,

    /// A cursor continues one order while the query asks for the other.
    #[error("The cursor continues a {stored} scan; the query asks for {requested}")]
    CursorOrder { stored: String, requested: String },

    /// A closed backend rejects every later operation.
    #[error("{0} is closed")]
    Closed(&'static str),

    /// Minting would leave the safe integer ID space.
    #[error("ID space is exhausted")]
    IdSpaceExhausted,

    /// `Tx` table reads after the first table write (upstream `ReadAfterWrite`).
    #[error("Tx.{0}() cannot read tables after the first table write")]
    ReadAfterWrite(&'static str),

    /// A submission reached a busy conversation.
    #[error("Conversation {0} is busy")]
    ConversationBusy(ConversationId),

    /// Corrupt complete JSONL line or marker.
    #[error("{0}")]
    JsonlCorruption(String),

    /// A JSONL storage whose write path failed and which must be reopened.
    #[error("JSONL storage is poisoned and must be reopened")]
    JsonlPoisoned(String),

    /// JSON encode/decode failure.
    #[error("{0}")]
    Json(#[from] serde_json::Error),

    /// Filesystem failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// SQLite failure.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Delta operation failure.
    #[error("{0}")]
    Delta(#[from] crate::chord::delta::DeltaError),

    /// Operation on a settled SQLite transaction handle.
    #[error("SQLite transaction handle is no longer active")]
    TransactionInactive,

    /// The SQLite facade was used from inside its own transaction callback,
    /// where upstream never settles. Replaced with an explicit error (form
    /// difference, V17-13 §7.3).
    #[error("SQLite database cannot be used from inside its own transaction callback")]
    TransactionReentrant,

    /// SQLite rollback failed after the callback failed; the callback error is
    /// not a guaranteed rollback (upstream `AggregateError`).
    #[error("SQLite transaction failed and rollback failed")]
    RollbackFailed {
        /// Original callback failure.
        #[source]
        callback: Box<StorageError>,
        /// Rollback failure.
        rollback: Box<StorageError>,
    },

    /// A filesystem seam failure carrying the upstream `FileError` code.
    #[error("File operation failed: {0}")]
    File(#[from] FileError),
}

impl StorageError {
    /// Build a generic validation error with the upstream message text.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    /// Build an `ID {id} already belongs to {table}` error.
    pub fn id_belongs_to<T>(id: Id<T>, table: &'static str) -> Self {
        Self::Message(format!("ID {id} already belongs to {table}"))
    }
    /// Whether this error describes a caller's malformed or disallowed request.
    pub fn is_request_error(&self) -> bool {
        matches!(
            self,
            Self::UnknownConversation(_)
                | Self::InvalidCursor
                | Self::CursorOrder { .. }
                | Self::ReadAfterWrite(_)
                | Self::ConversationBusy(_)
        )
    }
}

/// Filesystem seam error (port of the result-shaped `FileError` from `env/`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct FileError {
    /// Upstream `FileError.code`; `not_found` is control flow in JSONL recovery.
    pub code: FileErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Path the failure concerns, when known.
    pub path: Option<String>,
    /// Cause chain as text (upstream `{ cause }`).
    pub cause: Option<String>,
}

impl FileError {
    /// Build an error with the upstream code and message.
    pub fn new(code: FileErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            cause: None,
        }
    }

    /// Build an error attributed to `path`.
    pub fn with_path(
        code: FileErrorCode,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            path: Some(path.into()),
            cause: None,
        }
    }

    /// Attach the text of an underlying failure.
    pub fn caused_by(mut self, cause: impl std::fmt::Display) -> Self {
        self.cause = Some(cause.to_string());
        self
    }

    pub fn not_found(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::with_path(
            FileErrorCode::NotFound,
            format!("File not found: {path}"),
            path,
        )
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(FileErrorCode::Unknown, message)
    }
}

/// Upstream `FileError` codes (`env/index.ts:41-50`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileErrorCode {
    /// The operation was cancelled.
    Aborted,
    /// The path does not exist.
    NotFound,
    /// Access was refused.
    PermissionDenied,
    /// A path component is not a directory.
    NotDirectory,
    /// The path is a directory.
    IsDirectory,
    /// The path or the arguments are invalid.
    Invalid,
    /// The file system cannot provide the operation.
    NotSupported,
    /// Any other filesystem failure.
    Unknown,
}

/// Upstream `StorageRejected` marker used by error-matching tests.
pub type StorageRejected = StorageError;
/// Convenience result alias.
pub type StorageResult<T> = Result<T, StorageError>;
