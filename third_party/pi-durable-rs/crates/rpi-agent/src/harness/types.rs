//! Port of `packages/agent/src/harness/types.ts` @ pi 0.82.1 (2efa728) — the
//! T16 harness type layer (first block).
//!
//! Everything in upstream `types.ts` that is a type — or a pure function over
//! those types the harness needs before the harness class lands — lives here:
//! the error family, `AgentHarnessPhase`, the 22 harness-owned events, hook
//! result types, `SessionStorage` / `SessionRepo` / `FileSystem` / `Shell` /
//! `ExecutionEnv`, options, and the write-side entry union.
//!
//! Intentional differences:
//! - TS structural typing has no Rust equivalent: the harness `SessionTreeEntry`
//!   union is the shared `crate::session::SessionEntry` (one payload struct per
//!   entry type, all 11 variants; see `session.rs` header). `SessionStorage`
//!   methods take/return that union. `findEntries<TType>` loses its per-type
//!   extraction and takes the `entry.type` tag string instead.
//! - `AbortSignal` is `tokio_util::sync::CancellationToken` (same convention as
//!   `AgentTool::execute` in the crate-root `types.rs`). Event fields carrying a
//!   signal are `#[serde(skip)]`: the signal is runtime-only, never appears on
//!   the wire, and deserializing yields a fresh token.
//! - Error classes carry no `cause` chain (upstream `cause?: Error`): upstream
//!   wrappers already copy the cause text into `message`
//!   (`normalizeHarnessError`, agent-harness.ts:140-147), so callers lose
//!   nothing. Codes are typed enums whose `as_str()` literals match upstream
//!   exactly (the T07 `SessionError` used `&'static str`; values unchanged).
//! - The `ok` / `err` / `getOrThrow` / `getOrUndefined` / `toError` helpers
//!   (types.ts:23-56) are not ported — Rust's built-in `Result` and typed
//!   errors replace them.
//! - TS generic parameters (`TContext`, `TSkill`, `TPromptTemplate`, `TTool`)
//!   collapse to the concrete defaults on serde-facing types (events,
//!   `AgentHarnessResources`, `PendingSessionWrite`) — a Rust enum must have
//!   one serialized shape. `TContext` survives on the application-facing
//!   generics (`AgentHarnessTool`, `AgentHarnessOptions`, `TurnState`,
//!   `AgentHarnessSystemPrompt`, `AgentHarnessToolContextSource`), defaulting
//!   to `()` where upstream defaults to `undefined`.
//! - `writeFile` / `appendFile` take `&[u8]` where upstream accepts
//!   `string | Uint8Array` — the distinction is irrelevant at the storage
//!   boundary.
//! - `Session` (upstream `harness/session/session.ts`, re-exported from
//!   types.ts:516) becomes a trait with the full facade surface — read side,
//!   append methods, `moveTo`, and context building — implemented by the
//!   concrete `harness::session::session_facade::Session` (the T16 `session.rs`
//!   port of the class). The facade-level option types (`ContextEntryTransform`,
//!   `CustomEntryContextMessageProjector`, `SessionContextBuildOptions`,
//!   `AppendCompactionOptions`, `MoveToSummary`) live here too because the
//!   trait methods reference them; upstream defines them in session.ts.
//! - `PendingSessionWrite` has 11 forms (one per `SessionTreeEntry` member).
//! - `apply_stream_options_patch` (agent-harness.ts:94-134) is ported here
//!   because it defines the `AgentHarnessStreamOptionsPatch` semantics; it
//!   moves to the agent-harness port when that file lands.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use rpi_ai::models::Models;
use rpi_ai::types::{
    CacheRetention, ConstrainedSampling, ImageContent, Model, ToolResultContent, Transport, Usage,
    UserContent,
};
use rpi_ai::utils::retry::RetryPolicy;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::messages::AgentMessage;
use crate::session::{BranchSummaryEntry, CompactionEntry, CustomEntry, SessionEntry};
use crate::types::{
    AgentEvent, AgentToolResult, AgentToolUpdateCallback, QueueMode, ThinkingLevel,
    ToolExecutionMode,
};

// ---------------------------------------------------------------------------
// Errors (types.ts:146-266)
// ---------------------------------------------------------------------------

/// `FileKind = "file" | "directory" | "symlink"` (types.ts:147).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "directory")]
    Directory,
    #[serde(rename = "symlink")]
    Symlink,
}

/// `FileErrorCode` (types.ts:150-158) — stable, backend-independent file
/// error codes returned by [`FileSystem`] operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileErrorCode {
    Aborted,
    NotFound,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    Invalid,
    NotSupported,
    Unknown,
}

impl FileErrorCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            FileErrorCode::Aborted => "aborted",
            FileErrorCode::NotFound => "not_found",
            FileErrorCode::PermissionDenied => "permission_denied",
            FileErrorCode::NotDirectory => "not_directory",
            FileErrorCode::IsDirectory => "is_directory",
            FileErrorCode::Invalid => "invalid",
            FileErrorCode::NotSupported => "not_supported",
            FileErrorCode::Unknown => "unknown",
        }
    }
}

impl fmt::Display for FileErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `FileError` (types.ts:161-173) — error returned by [`FileSystem`]
/// operations. The upstream `path` and `cause` fields are dropped: the path is
/// normally part of the message, and the cause chain carries no information
/// beyond the message text.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("file error ({code}): {message}")]
pub struct FileError {
    pub code: FileErrorCode,
    pub message: String,
}

impl FileError {
    pub fn new(code: FileErrorCode, message: impl Into<String>) -> Self {
        FileError {
            code,
            message: message.into(),
        }
    }
}

/// `ExecutionErrorCode` (types.ts:176-182) — stable execution error codes
/// returned by [`ExecutionEnv::exec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    Unknown,
}

impl ExecutionErrorCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionErrorCode::Aborted => "aborted",
            ExecutionErrorCode::Timeout => "timeout",
            ExecutionErrorCode::ShellUnavailable => "shell_unavailable",
            ExecutionErrorCode::SpawnError => "spawn_error",
            ExecutionErrorCode::CallbackError => "callback_error",
            ExecutionErrorCode::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ExecutionErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `ExecutionError` (types.ts:185-194) — error returned by
/// [`ExecutionEnv::exec`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("execution error ({code}): {message}")]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
}

impl ExecutionError {
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        ExecutionError {
            code,
            message: message.into(),
        }
    }
}

/// `CompactionErrorCode` (types.ts:197) — stable compaction error codes
/// returned by compaction helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompactionErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
    Unknown,
}

impl CompactionErrorCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionErrorCode::Aborted => "aborted",
            CompactionErrorCode::SummarizationFailed => "summarization_failed",
            CompactionErrorCode::InvalidSession => "invalid_session",
            CompactionErrorCode::Unknown => "unknown",
        }
    }
}

impl fmt::Display for CompactionErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `CompactionError` (types.ts:200-209) — error returned by compaction
/// helpers.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("compaction error ({code}): {message}")]
pub struct CompactionError {
    pub code: CompactionErrorCode,
    pub message: String,
}

impl CompactionError {
    pub fn new(code: CompactionErrorCode, message: impl Into<String>) -> Self {
        CompactionError {
            code,
            message: message.into(),
        }
    }
}

/// `BranchSummaryErrorCode` (types.ts:212) — stable branch-summary error codes
/// returned by branch summarization helpers. Note: no `unknown` upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BranchSummaryErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
}

impl BranchSummaryErrorCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            BranchSummaryErrorCode::Aborted => "aborted",
            BranchSummaryErrorCode::SummarizationFailed => "summarization_failed",
            BranchSummaryErrorCode::InvalidSession => "invalid_session",
        }
    }
}

impl fmt::Display for BranchSummaryErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `BranchSummaryError` (types.ts:215-224) — error returned by branch
/// summarization helpers.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("branch summary error ({code}): {message}")]
pub struct BranchSummaryError {
    pub code: BranchSummaryErrorCode,
    pub message: String,
}

impl BranchSummaryError {
    pub fn new(code: BranchSummaryErrorCode, message: impl Into<String>) -> Self {
        BranchSummaryError {
            code,
            message: message.into(),
        }
    }
}

/// `SessionErrorCode` (types.ts:226-232) — session subsystem error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionErrorCode {
    NotFound,
    InvalidSession,
    InvalidEntry,
    InvalidForkTarget,
    Storage,
    Unknown,
}

impl SessionErrorCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionErrorCode::NotFound => "not_found",
            SessionErrorCode::InvalidSession => "invalid_session",
            SessionErrorCode::InvalidEntry => "invalid_entry",
            SessionErrorCode::InvalidForkTarget => "invalid_fork_target",
            SessionErrorCode::Storage => "storage",
            SessionErrorCode::Unknown => "unknown",
        }
    }
}

impl fmt::Display for SessionErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `SessionError` (types.ts:235-244) — error thrown by session storage,
/// repositories, and session tree operations. `code` values match upstream
/// exactly.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("session error ({code}): {message}")]
pub struct SessionError {
    pub code: SessionErrorCode,
    pub message: String,
}

impl SessionError {
    pub fn new(code: SessionErrorCode, message: impl Into<String>) -> Self {
        SessionError {
            code,
            message: message.into(),
        }
    }
}

/// `AgentHarnessErrorCode` (types.ts:246-255) — public `AgentHarness` failure
/// classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentHarnessErrorCode {
    Busy,
    InvalidState,
    InvalidArgument,
    Session,
    Hook,
    Auth,
    Compaction,
    BranchSummary,
    Unknown,
}

impl AgentHarnessErrorCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentHarnessErrorCode::Busy => "busy",
            AgentHarnessErrorCode::InvalidState => "invalid_state",
            AgentHarnessErrorCode::InvalidArgument => "invalid_argument",
            AgentHarnessErrorCode::Session => "session",
            AgentHarnessErrorCode::Hook => "hook",
            AgentHarnessErrorCode::Auth => "auth",
            AgentHarnessErrorCode::Compaction => "compaction",
            AgentHarnessErrorCode::BranchSummary => "branch_summary",
            AgentHarnessErrorCode::Unknown => "unknown",
        }
    }
}

impl fmt::Display for AgentHarnessErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `AgentHarnessError` (types.ts:257-266) — public `AgentHarness` failure with
/// a stable top-level classification.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("agent harness error ({code}): {message}")]
pub struct AgentHarnessError {
    pub code: AgentHarnessErrorCode,
    pub message: String,
}

impl AgentHarnessError {
    pub fn new(code: AgentHarnessErrorCode, message: impl Into<String>) -> Self {
        AgentHarnessError {
            code,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Stream options (types.ts:119-144) and the patch application semantics
// (agent-harness.ts:94-134)
// ---------------------------------------------------------------------------

/// `AgentHarnessStreamOptions` (types.ts:119-135) — curated provider request
/// options owned by the harness and snapshotted per turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessStreamOptions {
    /// Preferred transport forwarded to the stream function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    /// Provider request timeout in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Maximum provider retry attempts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Optional cap for provider-requested retry delays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    /// Additional request headers merged with auth and lifecycle headers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Provider metadata forwarded with requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
    /// Provider cache retention hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
}

/// `AgentHarnessStreamOptionsPatch` (types.ts:137-144) — per-request stream
/// option patch returned by provider hooks.
///
/// `None` on a scalar field means the key is absent from the patch (no
/// change), the Rust equivalent of upstream `Object.hasOwn(patch, key)` being
/// false. (An upstream explicit `undefined` for a scalar — indistinguishable
/// from absence on the wire — is not expressible; it is never used in
/// practice.) `headers` / `metadata` use the three-state [`PatchMap`]: absent
/// (no change), `Clear` (upstream explicit `undefined`), or `Merge` with
/// per-key deletion semantics (`None` values delete keys). Deletions and
/// `Clear` serialize as JSON `null` (upstream keeps them as in-memory
/// `undefined`, which `JSON.stringify` would drop — equivalent on the wire).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessStreamOptionsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    /// Header patch.
    #[serde(skip_serializing_if = "PatchMap::is_absent", default)]
    pub headers: PatchMap<String>,
    /// Metadata patch.
    #[serde(skip_serializing_if = "PatchMap::is_absent", default)]
    pub metadata: PatchMap<Value>,
}

/// Three-state patch field for `AgentHarnessStreamOptionsPatch.headers` /
/// `.metadata` (types.ts:140-143): upstream distinguishes key absence (no
/// change), explicit `undefined` (clear the whole map), and a map with
/// per-key `undefined` values (delete those keys). The explicit `undefined`
/// state exists in memory only; on the wire it is `null` (upstream's
/// `JSON.stringify` drops `undefined` entirely, so the states collapse there).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PatchMap<T> {
    /// Key absent from the patch object: no change.
    #[default]
    Absent,
    /// Explicit `undefined`: clears the whole map.
    Clear,
    /// Merge map; `None` values delete keys.
    Merge(BTreeMap<String, Option<T>>),
}

impl<T> PatchMap<T> {
    /// `skip_serializing_if` helper: `Absent` fields are omitted from JSON.
    pub fn is_absent(&self) -> bool {
        matches!(self, PatchMap::Absent)
    }
}

/// `applyStreamOptionsPatch` (agent-harness.ts:94-134) — apply a hook-returned
/// stream-option patch onto the turn's base options. The base is never
/// mutated; scalar fields are replaced when present in the patch, and
/// `headers` / `metadata` merge key-by-key with deletion semantics
/// ([`PatchMap::Merge`]) or clear entirely ([`PatchMap::Clear`], upstream
/// explicit `undefined`).
pub fn apply_stream_options_patch(
    base: &AgentHarnessStreamOptions,
    patch: Option<&AgentHarnessStreamOptionsPatch>,
) -> AgentHarnessStreamOptions {
    let mut result = base.clone();
    let Some(patch) = patch else {
        return result;
    };

    if patch.transport.is_some() {
        result.transport = patch.transport;
    }
    if patch.timeout_ms.is_some() {
        result.timeout_ms = patch.timeout_ms;
    }
    if patch.max_retries.is_some() {
        result.max_retries = patch.max_retries;
    }
    if patch.max_retry_delay_ms.is_some() {
        result.max_retry_delay_ms = patch.max_retry_delay_ms;
    }
    if patch.cache_retention.is_some() {
        result.cache_retention = patch.cache_retention;
    }

    match &patch.headers {
        PatchMap::Absent => {}
        // Upstream `patch.headers === undefined`: clear all headers.
        PatchMap::Clear => result.headers = None,
        PatchMap::Merge(entries) => {
            let mut headers = result.headers.clone().unwrap_or_default();
            for (key, value) in entries {
                match value {
                    Some(value) => {
                        headers.insert(key.clone(), value.clone());
                    }
                    None => {
                        headers.remove(key);
                    }
                }
            }
            // `Object.keys(headers).length > 0 ? headers : undefined`.
            result.headers = if headers.is_empty() {
                None
            } else {
                Some(headers)
            };
        }
    }

    match &patch.metadata {
        PatchMap::Absent => {}
        // Upstream `patch.metadata === undefined`: clear all metadata.
        PatchMap::Clear => result.metadata = None,
        PatchMap::Merge(entries) => {
            let mut metadata = result.metadata.clone().unwrap_or_default();
            for (key, value) in entries {
                match value {
                    Some(value) => {
                        metadata.insert(key.clone(), value.clone());
                    }
                    None => {
                        metadata.remove(key);
                    }
                }
            }
            result.metadata = if metadata.is_empty() {
                None
            } else {
                Some(metadata)
            };
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Filesystem and shell capabilities (types.ts:268-373)
// ---------------------------------------------------------------------------

/// `FileInfo` (types.ts:268-280) — metadata for one filesystem object in a
/// [`FileSystem`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    /// Basename of `path`.
    pub name: String,
    /// Absolute, syntactically normalized addressed path in the execution
    /// environment. Symlinks are not followed.
    pub path: String,
    /// Object kind. Symlink targets are not followed.
    pub kind: FileKind,
    /// Size in bytes for the addressed filesystem object.
    pub size: u64,
    /// Modification time as milliseconds since Unix epoch.
    pub mtime_ms: f64,
}

/// Options for [`FileSystem::read_text_lines`] (types.ts:301-305).
#[derive(Debug, Clone, Default)]
pub struct ReadTextLinesOptions {
    /// Implementations should stop once `max_lines` lines have been read.
    pub max_lines: Option<usize>,
    pub abort_signal: Option<CancellationToken>,
}

/// Options for [`FileSystem::create_dir`] (types.ts:320-324).
#[derive(Debug, Clone, Default)]
pub struct CreateDirOptions {
    /// Upstream default: `true`.
    pub recursive: Option<bool>,
    pub abort_signal: Option<CancellationToken>,
}

/// Options for [`FileSystem::remove`] (types.ts:325-329).
#[derive(Debug, Clone, Default)]
pub struct RemoveOptions {
    /// Upstream default: `false`.
    pub recursive: bool,
    /// Upstream default: `false`.
    pub force: bool,
    pub abort_signal: Option<CancellationToken>,
}

/// Options for [`FileSystem::create_temp_file`] (types.ts:332-337).
#[derive(Debug, Clone, Default)]
pub struct CreateTempFileOptions {
    /// Upstream default: `""`.
    pub prefix: Option<String>,
    /// Upstream default: `""`.
    pub suffix: Option<String>,
    pub abort_signal: Option<CancellationToken>,
}

/// `FileSystem` (types.ts:291-341) — filesystem capability used by the
/// harness.
///
/// Paths passed to methods may be absolute or relative to [`FileSystem::cwd`].
/// Paths returned by file operations are addressed paths in the filesystem
/// namespace, but are not canonicalized through symlinks unless returned by
/// [`FileSystem::canonical_path`].
///
/// Operation methods must never panic. All filesystem failures, including
/// unexpected backend failures, must be encoded in the returned [`Result`]
/// (upstream: "must never throw or reject"). Implementations must preserve
/// this invariant.
///
/// Upstream `abortSignal?: AbortSignal` becomes `Option<CancellationToken>`;
/// upstream `content: string | Uint8Array` becomes `&[u8]` (see header note).
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// Current working directory for relative paths.
    fn cwd(&self) -> &str;

    /// `absolutePath` — absolute addressed path without requiring the path to
    /// exist and without resolving symlinks.
    async fn absolute_path(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// `joinPath` — join path segments without requiring the result to exist.
    async fn join_path(
        &self,
        parts: &[String],
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// `readTextFile` — read a UTF-8 text file.
    async fn read_text_file(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// `readTextLines` — read UTF-8 text lines. Implementations should stop
    /// once `max_lines` lines have been read.
    async fn read_text_lines(
        &self,
        path: &str,
        options: ReadTextLinesOptions,
    ) -> Result<Vec<String>, FileError>;

    /// `readBinaryFile` — read a binary file.
    async fn read_binary_file(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<Vec<u8>, FileError>;

    /// `writeFile` — create or overwrite a file, creating parent directories
    /// when supported.
    async fn write_file(
        &self,
        path: &str,
        content: &[u8],
        abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError>;

    /// `appendFile` — create or append to a file, creating parent directories
    /// when supported.
    async fn append_file(
        &self,
        path: &str,
        content: &[u8],
        abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError>;

    /// `fileInfo` — metadata for the addressed path without following
    /// symlinks.
    async fn file_info(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<FileInfo, FileError>;

    /// `listDir` — direct children of a directory without following symlinks.
    async fn list_dir(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<Vec<FileInfo>, FileError>;

    /// `canonicalPath` — canonical path for an existing path, resolving
    /// symlinks where supported.
    async fn canonical_path(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// `exists` — `false` for missing paths. Other errors, such as permission
    /// failures, return a [`FileError`].
    async fn exists(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<bool, FileError>;

    /// `createDir`. Defaults: `recursive: true`, no abort signal.
    async fn create_dir(&self, path: &str, options: CreateDirOptions) -> Result<(), FileError>;

    /// `remove` — remove a file or directory. Defaults: `recursive: false`,
    /// `force: false`, no abort signal.
    async fn remove(&self, path: &str, options: RemoveOptions) -> Result<(), FileError>;

    /// `renameFile` (harness/types.ts:252-253 @ 4181f66) — atomically rename a
    /// file, replacing the destination when it exists. Does not copy across
    /// filesystems. Reserved for atomic publish (R4.3.2 prevision); no callers
    /// are wired yet.
    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError>;

    /// `createTempDir` — create a temporary directory and return its absolute
    /// path. Defaults: `prefix: "tmp-"`, no abort signal.
    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// `createTempFile` — create a temporary file and return its absolute
    /// path. Defaults: `prefix: ""`, `suffix: ""`, no abort signal.
    async fn create_temp_file(&self, options: CreateTempFileOptions) -> Result<String, FileError>;

    /// `cleanup` — release filesystem resources. Must be best-effort and must
    /// not fail.
    async fn cleanup(&self);
}

/// `(chunk: string) => void` chunk callback of [`ShellExecOptions`]
/// (types.ts:356-358).
pub type ChunkCallback = Box<dyn Fn(&str) + Send + Sync>;

/// `ShellExecOptions` (types.ts:343-359) — options for [`Shell::exec`].
///
/// The chunk callbacks are plain [`ChunkCallback`] boxes, so the struct is
/// neither `Clone` nor `Debug`.
#[derive(Default)]
pub struct ShellExecOptions {
    /// Working directory for the command. Relative paths are resolved against
    /// [`ExecutionEnv::cwd`]. Defaults to the execution env's cwd.
    pub cwd: Option<String>,
    /// Environment variables for the command. Values override inherited
    /// defaults when `inherit_env` is true.
    pub env: Option<BTreeMap<String, String>>,
    /// Whether to inherit the execution environment's default variables.
    /// Upstream default: `true`.
    pub inherit_env: Option<bool>,
    /// Timeout in seconds. Implementations should return a timeout error when
    /// the command exceeds this duration. Upstream default: no timeout.
    pub timeout: Option<u64>,
    /// Abort signal used to terminate the command. Upstream default: no abort
    /// signal.
    pub abort_signal: Option<CancellationToken>,
    /// Called with stdout chunks as they are produced.
    pub on_stdout: Option<ChunkCallback>,
    /// Called with stderr chunks as they are produced.
    pub on_stderr: Option<ChunkCallback>,
}

/// Result of [`Shell::exec`] (types.ts:367).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// `Shell` (types.ts:362-370) — shell execution capability used by the
/// harness.
#[async_trait]
pub trait Shell: Send + Sync {
    /// `exec` — execute a shell command in [`FileSystem::cwd`] unless
    /// `options.cwd` is provided.
    async fn exec(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
    ) -> Result<ShellExecResult, ExecutionError>;

    /// `cleanup` — release shell resources. Must be best-effort and must not
    /// fail.
    async fn cleanup(&self);
}

/// `ExecutionEnv` (types.ts:373) — filesystem and process execution
/// environment used by the harness. Blanket-implemented for any type
/// satisfying both [`FileSystem`] and [`Shell`].
pub trait ExecutionEnv: FileSystem + Shell {}

impl<T: FileSystem + Shell> ExecutionEnv for T {}

// ---------------------------------------------------------------------------
// Session context, stats, and metadata (types.ts:466-496)
// ---------------------------------------------------------------------------

/// `SessionContext` (types.ts:466-471).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    /// Upstream `{ provider: string; modelId: string } | null`.
    pub model: Option<SessionModelRef>,
    /// Upstream `string[] | null`.
    pub active_tool_names: Option<Vec<String>>,
}

/// Inline `{ provider: string; modelId: string }` object of [`SessionContext`]
/// (types.ts:469).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelRef {
    pub provider: String,
    pub model_id: String,
}

/// `SessionStats` (types.ts:473-479).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub message_count: u64,
    pub cached_tokens: u64,
    pub uncached_tokens: u64,
    pub total_tokens: u64,
    pub cost_total: f64,
}

/// `SessionMetadata` (types.ts:481-484).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: String,
}

/// `JsonlSessionMetadata` (types.ts:486-491). The upstream inheritance
/// (`extends SessionMetadata`) is expressed with `#[serde(flatten)]`, so the
/// wire shape is flat and camelCase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlSessionMetadata {
    #[serde(flatten)]
    pub base: SessionMetadata,
    pub cwd: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

/// `SessionEntryCursorOptions` (types.ts:493-496).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntryCursorOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_entry_seq: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Session facade option types (session/session.ts)
// ---------------------------------------------------------------------------

/// `ContextEntryTransform` (session.ts:24) — entry-list → entry-list transform
/// applied during context building. Upstream's TS function type is an
/// `Arc<dyn Fn + Send + Sync>` here (`Arc` so merged option sets can clone
/// shared transforms, session.ts:194).
pub type ContextEntryTransform = Arc<dyn Fn(&[SessionEntry]) -> Vec<SessionEntry> + Send + Sync>;

/// `CustomEntryContextMessageProjector` (session.ts:26-30) — projects a
/// `custom` entry into context messages; an empty vec omits the entry
/// (upstream `undefined` and `[]` both mean "no messages").
pub type CustomEntryContextMessageProjector =
    Arc<dyn Fn(&CustomEntry, usize, &[SessionEntry]) -> Vec<AgentMessage> + Send + Sync>;

/// `SessionContextBuildOptions` (session.ts:32-37).
#[derive(Default)]
pub struct SessionContextBuildOptions {
    /// Additional entry transforms applied after the default compaction
    /// transform.
    pub entry_transforms: Vec<ContextEntryTransform>,
    /// Optional custom-entry projectors. Custom entries are omitted from
    /// model context by default.
    pub entry_projectors: HashMap<String, CustomEntryContextMessageProjector>,
}

/// Trailing options of `appendCompaction` (session.ts:260-281) — the upstream
/// `details?` / `fromHook?` / `usage?` / `retainedTail?` parameters.
#[derive(Debug, Clone, Default)]
pub struct AppendCompactionOptions {
    /// Extension-specific data (e.g. ArtifactIndex).
    pub details: Option<Value>,
    /// True if generated by an extension.
    pub from_hook: Option<bool>,
    /// Usage from the LLM call(s) that generated this summary, if available.
    pub usage: Option<Usage>,
    /// Self-contained retained tail messages (harness form; when present the
    /// compaction is a checkpoint and the walk stops there).
    pub retained_tail: Option<Vec<AgentMessage>>,
}

/// `moveTo` summary parameter (session.ts:338-340) — when present, `moveTo`
/// appends a `branch_summary` entry after moving the leaf.
#[derive(Debug, Clone, Default)]
pub struct MoveToSummary {
    pub summary: String,
    /// Extension-specific data (not sent to LLM).
    pub details: Option<Value>,
    /// Usage from the LLM call that generated this summary, if available.
    pub usage: Option<Usage>,
    /// True if generated by an extension.
    pub from_hook: Option<bool>,
}

// ---------------------------------------------------------------------------
// Session storage and repository (types.ts:498-551)
// ---------------------------------------------------------------------------

/// `SessionStorage` (types.ts:498-514) — storage backend contract for the
/// harness session tree (ports of `jsonl-storage.ts` / `memory-storage.ts` in
/// `harness/session/`).
///
/// Upstream `SessionStorage<TMetadata>`'s generic metadata becomes an
/// associated type (Rust idiom). Upstream `Promise` rejections become
/// `Result<_, SessionError>`; the `SessionError.code` strings match upstream
/// exactly. `appendEntry` takes the unified [`SessionEntry`] union (the
/// harness `SessionTreeEntry`; see `session.rs` header).
///
/// Write methods take `&self`: the `Session` facade (session.ts) calls them
/// through `Arc<dyn SessionStorage>` (upstream `getStorage` returns the class
/// instance, whose methods mutate internal state). The two implementations
/// use interior mutability (`tokio::sync::Mutex`) to match.
#[async_trait]
pub trait SessionStorage: Send + Sync {
    /// Upstream `TMetadata` generic parameter.
    type Metadata;

    /// `getMetadata`.
    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError>;

    /// `getLeafId`.
    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError>;

    /// `setLeafId` — persist a leaf entry recording the active tree leaf.
    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError>;

    /// `createEntryId`.
    async fn create_entry_id(&self) -> Result<String, SessionError>;

    /// `appendEntry`.
    async fn append_entry(&self, entry: SessionEntry) -> Result<(), SessionError>;

    /// `getEntry`.
    async fn get_entry(&self, id: &str) -> Result<Option<SessionEntry>, SessionError>;

    /// `findEntries<TType>` — filter by `entry.type` tag (upstream's per-type
    /// extraction has no Rust equivalent; callers match on variants).
    async fn find_entries(&self, entry_type: &str) -> Result<Vec<SessionEntry>, SessionError>;

    /// `getLabel`.
    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError>;

    /// `getSessionName`.
    async fn get_session_name(&self) -> Result<Option<String>, SessionError>;

    /// `getSessionStats`.
    async fn get_session_stats(&self) -> Result<SessionStats, SessionError>;

    /// `getPathToRootOrCompaction`.
    async fn get_path_to_root_or_compaction(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionEntry>, SessionError>;

    /// `getEntries` — upstream `options?`; pass [`SessionEntryCursorOptions`]
    /// by value, `Default::default()` for the no-options call.
    async fn get_entries(
        &self,
        options: SessionEntryCursorOptions,
    ) -> Result<Vec<SessionEntry>, SessionError>;
}

/// `Session` (harness/session/session.ts:150-358; re-exported from
/// types.ts:516) — facade over a [`SessionStorage`], as a trait.
///
/// Full upstream class surface: read side, append methods, `moveTo`, and
/// context building. Implemented by the concrete
/// `harness::session::session_facade::Session` struct (the T16 port of the
/// class), which `toSession` (repo-utils.ts:20-22) wraps storage backends
/// with.
#[async_trait]
pub trait Session: Send + Sync {
    /// Upstream `TMetadata` generic parameter.
    type Metadata;

    /// `getStorage`.
    fn storage(&self) -> Arc<dyn SessionStorage<Metadata = Self::Metadata>>;

    /// `getMetadata`.
    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError>;

    /// `getLeafId`.
    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError>;

    /// `getEntry`.
    async fn get_entry(&self, id: &str) -> Result<Option<SessionEntry>, SessionError>;

    /// `getEntries`.
    async fn get_entries(
        &self,
        options: SessionEntryCursorOptions,
    ) -> Result<Vec<SessionEntry>, SessionError>;

    /// `getBranch` — path from `from_id` (or the leaf) to the root or the
    /// latest compaction (session.ts:179-182).
    async fn get_branch(&self, from_id: Option<&str>) -> Result<Vec<SessionEntry>, SessionError>;

    /// `getLabel`.
    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError>;

    /// `getSessionName`.
    async fn get_session_name(&self) -> Result<Option<String>, SessionError>;

    /// `getSessionStats`.
    async fn get_session_stats(&self) -> Result<SessionStats, SessionError>;

    /// `appendMessage` (session.ts:219-227) — appends a `message` entry under
    /// the current leaf; returns the new entry id.
    async fn append_message(&self, message: AgentMessage) -> Result<String, SessionError>;

    /// `appendThinkingLevelChange` (session.ts:229-237).
    async fn append_thinking_level_change(
        &self,
        thinking_level: &str,
    ) -> Result<String, SessionError>;

    /// `appendModelChange` (session.ts:239-248).
    async fn append_model_change(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, SessionError>;

    /// `appendActiveToolsChange` (session.ts:250-258).
    async fn append_active_tools_change(
        &self,
        active_tool_names: &[String],
    ) -> Result<String, SessionError>;

    /// `appendCompaction` (session.ts:260-282) — `first_kept_entry_id` /
    /// `tokens_before` plus the trailing [`AppendCompactionOptions`].
    async fn append_compaction(
        &self,
        summary: &str,
        first_kept_entry_id: Option<&str>,
        tokens_before: u64,
        options: AppendCompactionOptions,
    ) -> Result<String, SessionError>;

    /// `appendCustomEntry` (session.ts:284-293) — extension data persisting
    /// across reloads; not part of LLM context by default.
    async fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Option<Value>,
    ) -> Result<String, SessionError>;

    /// `appendCustomMessageEntry` (session.ts:295-311) — extension message
    /// injected into LLM context.
    async fn append_custom_message_entry(
        &self,
        custom_type: &str,
        content: UserContent,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, SessionError>;

    /// `appendLabel` (session.ts:313-325) — rejects missing targets with
    /// `not_found`.
    async fn append_label(
        &self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, SessionError>;

    /// `appendSessionName` (session.ts:327-336) — `[\r\n]+` runs collapse to a
    /// single space, then the name is trimmed.
    async fn append_session_name(&self, name: &str) -> Result<String, SessionError>;

    /// `moveTo` (session.ts:338-358) — `entry_id: None` moves the leaf to the
    /// root; with a `summary` a `branch_summary` entry is appended (returning
    /// its id), otherwise `None`.
    async fn move_to(
        &self,
        entry_id: Option<&str>,
        summary: Option<MoveToSummary>,
    ) -> Result<Option<String>, SessionError>;

    /// `buildContextEntries` (session.ts:184-186) — branch entries after the
    /// default compaction transform plus the configured entry transforms.
    async fn build_context_entries(
        &self,
        options: SessionContextBuildOptions,
    ) -> Result<Vec<SessionEntry>, SessionError>;

    /// `buildContext` (session.ts:188-190) — derived thinking level / model /
    /// active tool names plus the projected context messages.
    async fn build_context(
        &self,
        options: SessionContextBuildOptions,
    ) -> Result<SessionContext, SessionError>;
}

/// `SessionCreateOptions` (types.ts:518-520).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `SessionForkOptions` (types.ts:522-526).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ForkPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `SessionForkOptions["position"]` (types.ts:524).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ForkPosition {
    /// Fork *before* the target entry; the target must be a user message
    /// (repo-utils.ts:42-48).
    #[default]
    #[serde(rename = "before")]
    Before,
    /// Fork *at* the target entry (included).
    #[serde(rename = "at")]
    At,
}

/// `SessionRepo` (types.ts:528-538).
///
/// Upstream `fork` takes `SessionForkOptions & TCreateOptions`; Rust cannot
/// express the intersection, so the options parameter is `TCreateOptions` —
/// implementations define a fork-options type combining both (e.g.
/// `JsonlSessionForkOptions`). Upstream `list(options?: TListOptions)` takes
/// `TListOptions` by value; the default `()` is upstream `void`.
#[async_trait]
pub trait SessionRepo<TMetadata, TCreateOptions = SessionCreateOptions, TListOptions = ()>:
    Send + Sync
{
    /// `create`.
    async fn create(
        &self,
        options: TCreateOptions,
    ) -> Result<Arc<dyn Session<Metadata = TMetadata>>, SessionError>;

    /// `open`.
    async fn open(
        &self,
        metadata: TMetadata,
    ) -> Result<Arc<dyn Session<Metadata = TMetadata>>, SessionError>;

    /// `list`.
    async fn list(&self, options: TListOptions) -> Result<Vec<TMetadata>, SessionError>;

    /// `delete`.
    async fn delete(&self, metadata: TMetadata) -> Result<(), SessionError>;

    /// `fork`.
    async fn fork(
        &self,
        source: TMetadata,
        options: TCreateOptions,
    ) -> Result<Arc<dyn Session<Metadata = TMetadata>>, SessionError>;
}

/// `JsonlSessionCreateOptions` (types.ts:540-544). The upstream inheritance is
/// expressed with `#[serde(flatten)]` (flat camelCase wire shape).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlSessionCreateOptions {
    #[serde(flatten)]
    pub base: SessionCreateOptions,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

/// `JsonlSessionListOptions` (types.ts:546-548).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlSessionListOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `JsonlSessionRepoApi` (types.ts:550-551) — `SessionRepo` over the JSONL
/// metadata/options. Use as `Arc<dyn JsonlSessionRepoApi>`.
pub type JsonlSessionRepoApi =
    dyn SessionRepo<JsonlSessionMetadata, JsonlSessionCreateOptions, JsonlSessionListOptions>;

// ---------------------------------------------------------------------------
// Phase and pending writes (types.ts:553-559)
// ---------------------------------------------------------------------------

/// `AgentHarnessPhase = "idle" | "turn" | "compaction" | "branch_summary" | "retry"`
/// (types.ts:553).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentHarnessPhase {
    #[default]
    Idle,
    Turn,
    Compaction,
    BranchSummary,
    /// Vestigial upstream: `"retry"` exists in the phase union but is never
    /// assigned by `AgentHarness` (agent-harness.ts) — kept for parity.
    Retry,
}

impl AgentHarnessPhase {
    /// Upstream phase literal.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentHarnessPhase::Idle => "idle",
            AgentHarnessPhase::Turn => "turn",
            AgentHarnessPhase::Compaction => "compaction",
            AgentHarnessPhase::BranchSummary => "branch_summary",
            AgentHarnessPhase::Retry => "retry",
        }
    }
}

/// `PendingSessionWrite` (types.ts:555-559) — [`SessionEntry`] minus
/// `id` / `parentId` / `timestamp`, i.e. the 11 entry forms the harness stages
/// before persisting. `type` tags and payload field names match the persisted
/// entry wire shapes (crate `session.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PendingSessionWrite {
    Message {
        message: AgentMessage,
    },
    ThinkingLevelChange {
        thinking_level: String,
    },
    ModelChange {
        provider: String,
        model_id: String,
    },
    ActiveToolsChange {
        active_tool_names: Vec<String>,
    },
    Compaction {
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        first_kept_entry_id: Option<String>,
        tokens_before: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        retained_tail: Option<Vec<AgentMessage>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    BranchSummary {
        from_id: String,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    Custom {
        custom_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    CustomMessage {
        custom_type: String,
        content: UserContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        display: bool,
    },
    Label {
        target_id: String,
        /// `string | undefined` upstream: omitted from JSON when `None`.
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    SessionInfo {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Leaf {
        /// `string | null` upstream: always serialized.
        target_id: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Skills, prompt templates, resources (types.ts:58-96)
// ---------------------------------------------------------------------------

/// `Skill` (types.ts:64-75) — skill loaded from a `SKILL.md` file or provided
/// by an application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// Stable skill name used for lookup and model-visible listings.
    pub name: String,
    /// Short model-visible description of when to use the skill.
    pub description: String,
    /// Full skill instructions.
    pub content: String,
    /// Absolute path to the skill file.
    pub file_path: String,
    /// Exclude this skill from model-visible skill lists while still allowing
    /// explicit application invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_model_invocation: Option<bool>,
}

/// `PromptTemplate` (types.ts:78-85) — prompt template that can be formatted
/// into a prompt for explicit invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    /// Stable template name used for lookup or application command routing.
    pub name: String,
    /// Optional description for command lists or autocomplete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Template content. Argument placeholders are formatted by
    /// `formatPromptTemplateInvocation`.
    pub content: String,
}

/// `AgentHarnessResources` (types.ts:88-96) — resources made available to
/// explicit invocation methods and system-prompt callbacks.
///
/// Upstream is generic over `TSkill` / `TPromptTemplate`; the defaults are
/// used in the serde-facing surface (events, options) — see header note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessResources<TSkill = Skill, TPromptTemplate = PromptTemplate> {
    /// Prompt templates available for explicit invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_templates: Option<Vec<TPromptTemplate>>,
    /// Skills available to the model and explicit skill invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<TSkill>>,
}

impl<TSkill, TPromptTemplate> Default for AgentHarnessResources<TSkill, TPromptTemplate> {
    fn default() -> Self {
        AgentHarnessResources {
            prompt_templates: None,
            skills: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tools (types.ts:98-117)
// ---------------------------------------------------------------------------

/// `AgentHarnessTool` (types.ts:98-112) — `Omit<AgentTool, "execute">` plus an
/// `execute` receiving the context resolved for the current turn snapshot.
///
/// Upstream `TContext extends object | undefined = undefined` becomes the
/// defaulted generic parameter `TContext = ()`. `TParameters` (a TypeBox
/// schema upstream) collapses to `serde_json::Value` like the crate-root
/// `AgentTool` (see `types.rs` header).
#[async_trait]
pub trait AgentHarnessTool<TContext = ()>: Send + Sync {
    /// Tool name (as seen by the model).
    fn name(&self) -> &str;
    /// Human-readable label for UI display.
    fn label(&self) -> &str;
    /// Tool description sent to the model.
    fn description(&self) -> &str;
    /// JSON Schema of the tool parameters.
    fn parameters(&self) -> &Value;
    /// Optional provider-side constrained sampling config.
    fn constrained_sampling(&self) -> Option<ConstrainedSampling> {
        None
    }
    /// Per-tool execution mode override; `None` applies the default mode.
    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        None
    }
    /// Optional compatibility shim for raw tool-call arguments before schema
    /// validation. Must return a value matching the parameters schema.
    /// Default: identity.
    fn prepare_arguments(&self, args: Value) -> Value {
        args
    }
    /// `execute` — execute the tool call with the context resolved for the
    /// current turn snapshot. Return `Err` on failure instead of encoding
    /// errors in `content` (same convention as [`AgentToolResult`]).
    async fn execute(
        &self,
        tool_call_id: &str,
        params: Value,
        signal: CancellationToken,
        on_update: Option<AgentToolUpdateCallback>,
        context: TContext,
    ) -> Result<AgentToolResult, AgentError>;
}

/// `AgentHarnessToolContextSource` (types.ts:114-117) — static tool context or
/// zero-argument provider resolved for each turn snapshot.
#[derive(Clone)]
pub enum AgentHarnessToolContextSource<TContext = ()> {
    /// Upstream `TContext`.
    Static(TContext),
    /// Upstream `() => TContext | Promise<TContext>` (async providers use the
    /// boxed-future form).
    Provider(Arc<dyn Fn() -> BoxFuture<'static, TContext> + Send + Sync>),
}

// ---------------------------------------------------------------------------
// Harness-owned events (types.ts:561-740)
// ---------------------------------------------------------------------------

/// `RetryOperation = "compaction" | "branch_summary"` (types.ts:667).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryOperation {
    #[serde(rename = "compaction")]
    Compaction,
    #[serde(rename = "branch_summary")]
    BranchSummary,
}

/// `"set" | "restore"` — source of a model/tools update
/// (types.ts:688, types.ts:703).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateSource {
    #[serde(rename = "set")]
    Set,
    #[serde(rename = "restore")]
    Restore,
}

/// `AgentHarnessOwnEvent` (types.ts:715-740) — events emitted by the harness
/// for extensions and UI, all 22 variants.
///
/// Serde shape is the RPC / JSON-mode wire format (coding-standards §4.4):
/// `type` tag + camelCase payload fields, mirroring `AgentEvent` in the
/// crate-root `types.rs`. `signal` fields are runtime-only and skipped on the
/// wire (see header note).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentHarnessOwnEvent {
    // Queue and lifecycle
    /// `QueueUpdateEvent` (types.ts:561-566).
    QueueUpdate {
        steer: Vec<AgentMessage>,
        follow_up: Vec<AgentMessage>,
        next_turn: Vec<AgentMessage>,
    },
    /// `SavePointEvent` (types.ts:568-571).
    SavePoint { had_pending_mutations: bool },
    /// `AbortEvent` (types.ts:573-577).
    Abort {
        cleared_steer: Vec<AgentMessage>,
        cleared_follow_up: Vec<AgentMessage>,
    },
    /// `SettledEvent` (types.ts:579-582).
    Settled { next_turn_count: u64 },
    /// `BeforeAgentStartEvent` (types.ts:584-593).
    BeforeAgentStart {
        prompt: String,
        images: Option<Vec<ImageContent>>,
        system_prompt: String,
        resources: AgentHarnessResources,
    },
    /// `ContextEvent` (types.ts:595-598).
    Context { messages: Vec<AgentMessage> },
    // Provider request lifecycle
    /// `BeforeProviderRequestEvent` (types.ts:600-605).
    ///
    /// `model` is boxed to keep the enum small (`clippy::large_enum_variant`);
    /// the serde shape is unchanged.
    BeforeProviderRequest {
        model: Box<Model>,
        session_id: String,
        stream_options: AgentHarnessStreamOptions,
    },
    /// `BeforeProviderPayloadEvent` (types.ts:607-611).
    ///
    /// `model` is boxed to keep the enum small; the serde shape is unchanged.
    BeforeProviderPayload { model: Box<Model>, payload: Value },
    /// `AfterProviderResponseEvent` (types.ts:613-617).
    AfterProviderResponse {
        status: u16,
        headers: BTreeMap<String, String>,
    },
    // Tool lifecycle
    /// `ToolCallEvent` (types.ts:619-624).
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        input: Map<String, Value>,
    },
    /// `ToolResultEvent` (types.ts:626-635).
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        input: Map<String, Value>,
        content: Vec<ToolResultContent>,
        details: Value,
        is_error: bool,
        usage: Option<Usage>,
    },
    // Session lifecycle
    /// `SessionBeforeCompactEvent` (types.ts:637-643).
    SessionBeforeCompact {
        preparation: CompactionPreparation,
        branch_entries: Vec<SessionEntry>,
        custom_instructions: Option<String>,
        /// Upstream `signal: AbortSignal` — runtime-only.
        #[serde(skip)]
        signal: CancellationToken,
    },
    /// `SessionCompactEvent` (types.ts:645-649).
    SessionCompact {
        compaction_entry: CompactionEntry,
        from_hook: bool,
    },
    /// `SessionBeforeTreeEvent` (types.ts:651-655).
    SessionBeforeTree {
        preparation: TreePreparation,
        /// Upstream `signal: AbortSignal` — runtime-only.
        #[serde(skip)]
        signal: CancellationToken,
    },
    /// `SessionTreeEvent` (types.ts:657-663). `newLeafId` / `oldLeafId` are
    /// `string | null` upstream (always serialized); `summaryEntry` /
    /// `fromHook` are optional (omitted when absent).
    SessionTree {
        new_leaf_id: Option<String>,
        old_leaf_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary_entry: Option<BranchSummaryEntry>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    // Retry lifecycle
    /// `RetryScheduledEvent` (types.ts:665-672).
    RetryScheduled {
        operation: RetryOperation,
        attempt: u64,
        max_attempts: u64,
        delay_ms: u64,
        error_message: String,
    },
    /// `RetryAttemptStartEvent` (types.ts:674-677).
    RetryAttemptStart { operation: RetryOperation },
    /// `RetryFinishedEvent` (types.ts:679-682).
    RetryFinished { operation: RetryOperation },
    // Config updates
    /// `ModelUpdateEvent` (types.ts:684-689).
    ///
    /// `model` / `previous_model` are boxed to keep the enum small; the serde
    /// shape is unchanged.
    ModelUpdate {
        model: Box<Model>,
        previous_model: Option<Box<Model>>,
        source: UpdateSource,
    },
    /// `ThinkingLevelUpdateEvent` (types.ts:691-695).
    ThinkingLevelUpdate {
        level: ThinkingLevel,
        previous_level: ThinkingLevel,
    },
    /// `ToolsUpdateEvent` (types.ts:697-704).
    ToolsUpdate {
        tool_names: Vec<String>,
        previous_tool_names: Vec<String>,
        active_tool_names: Vec<String>,
        previous_active_tool_names: Vec<String>,
        source: UpdateSource,
    },
    /// `ResourcesUpdateEvent` (types.ts:706-713).
    ResourcesUpdate {
        resources: AgentHarnessResources,
        previous_resources: AgentHarnessResources,
    },
}

/// `AgentHarnessEvent = AgentEvent | AgentHarnessOwnEvent` (types.ts:742-744).
///
/// The tag sets are disjoint, so an untagged union roundtrips to the same
/// flat JSON an upstream event object has (no wrapper).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentHarnessEvent {
    /// `AgentEvent` (agent loop events).
    Agent(AgentEvent),
    /// `AgentHarnessOwnEvent` (harness events).
    Harness(AgentHarnessOwnEvent),
}

// ---------------------------------------------------------------------------
// Hook results (types.ts:746-817)
// ---------------------------------------------------------------------------

/// `BeforeAgentStartResult` (types.ts:746-749).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<AgentMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// `ContextResult` (types.ts:751-753) — full messages override.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextResult {
    pub messages: Vec<AgentMessage>,
}

/// `BeforeProviderRequestResult` (types.ts:755-757).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderRequestResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<AgentHarnessStreamOptionsPatch>,
}

/// `BeforeProviderPayloadResult` (types.ts:759-761) — full payload
/// replacement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderPayloadResult {
    pub payload: Value,
}

/// `ToolCallResult` (types.ts:763-766).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `ToolResultPatch` (types.ts:768-774) — partial tool-result override; absent
/// fields keep the original value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolResultContent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// `SessionBeforeCompactResult` (types.ts:776-779).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeCompactResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactResult>,
}

/// Inline `summary` object of [`SessionBeforeTreeResult`] (types.ts:783-788).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeSummary {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Usage from the LLM call that generated this summary, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// `SessionBeforeTreeResult` (types.ts:781-792).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeTreeResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<TreeSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `AgentHarnessEventResultMap` (types.ts:794-817) — per-event hook return
/// values, as an enum keyed by event.
///
/// Each result-bearing variant carries the hook's return value
/// (`Option`, mirroring upstream `Result | undefined`); the events whose map
/// values are fixed `undefined` map to [`HarnessHookResult::NoResult`].
#[derive(Debug, Clone, PartialEq, Default)]
pub enum HarnessHookResult {
    /// `before_agent_start` → `BeforeAgentStartResult | undefined`.
    BeforeAgentStart(Option<BeforeAgentStartResult>),
    /// `context` → `ContextResult | undefined`.
    Context(Option<ContextResult>),
    /// `before_provider_request` → `BeforeProviderRequestResult | undefined`.
    BeforeProviderRequest(Option<BeforeProviderRequestResult>),
    /// `before_provider_payload` → `BeforeProviderPayloadResult | undefined`.
    BeforeProviderPayload(Option<BeforeProviderPayloadResult>),
    /// `tool_call` → `ToolCallResult | undefined`.
    ToolCall(Option<ToolCallResult>),
    /// `tool_result` → `ToolResultPatch | undefined`.
    ToolResult(Option<ToolResultPatch>),
    /// `session_before_compact` → `SessionBeforeCompactResult | undefined`.
    SessionBeforeCompact(Option<SessionBeforeCompactResult>),
    /// `session_before_tree` → `SessionBeforeTreeResult | undefined`.
    SessionBeforeTree(Option<SessionBeforeTreeResult>),
    /// Events whose hooks return `undefined` upstream:
    /// `after_provider_response`, `session_compact`, `session_tree`,
    /// `retry_scheduled`, `retry_attempt_start`, `retry_finished`,
    /// `model_update`, `thinking_level_update`, `resources_update`,
    /// `tools_update`, `queue_update`, `save_point`, `abort`, `settled`.
    #[default]
    NoResult,
}

// ---------------------------------------------------------------------------
// Prompt / compaction / tree result types (types.ts:819-894)
// ---------------------------------------------------------------------------

/// `AgentHarnessPromptOptions` (types.ts:819-821).
#[derive(Debug, Clone, Default)]
pub struct AgentHarnessPromptOptions {
    pub images: Option<Vec<ImageContent>>,
}

/// `AbortResult` (types.ts:823-826).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortResult {
    pub cleared_steer: Vec<AgentMessage>,
    pub cleared_follow_up: Vec<AgentMessage>,
}

/// `CompactResult` (types.ts:828-836) — result of a harness compaction run
/// (distinct from the coding-agent `CompactionResult` in `compaction.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactResult {
    pub summary: String,
    pub tokens_before: u64,
    /// Usage from the LLM call(s) that generated this summary, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Self-contained retained tail (44289550a @ 4181f66: required; the
    /// `firstKeptEntryId` anchor was removed upstream). `#[serde(default)]`
    /// keeps the extension ABI boundary tolerant of older hook payloads.
    #[serde(default)]
    pub retained_tail: Vec<AgentMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// `NavigateTreeResult` (types.ts:838-842).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateTreeResult {
    pub cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_entry: Option<BranchSummaryEntry>,
}

/// `CompactionSettings` (types.ts:844-848) — identical shape to the
/// coding-agent `compaction.ts` port; re-used instead of redefined.
pub use crate::compaction::CompactionSettings;

/// `FileOperations` (types.ts:862-866) — same three sets as the utils.ts port
/// (`crate::compaction::utils`); the serde derives there serve this
/// re-export, which rides inside event payloads.
pub use crate::compaction::utils::FileOperations;

/// `CompactionPreparation` (types.ts:850-860) — harness variant of the
/// preparation, distinct from `crate::compaction::CompactionPreparation`
/// (coding-agent port): includes `retainedTail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPreparation {
    /// Entry id where retained history starts.
    pub first_kept_entry_id: String,
    /// Messages summarized into the history summary.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Prefix messages summarized separately when compaction splits a turn.
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Recent messages retained after compaction and stored on the compaction
    /// entry.
    pub retained_tail: Vec<AgentMessage>,
    /// Whether compaction splits a turn.
    pub is_split_turn: bool,
    /// Estimated context tokens before compaction.
    pub tokens_before: u64,
    /// Previous compaction summary used for iterative updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_summary: Option<String>,
    /// File operations extracted from summarized history.
    pub file_ops: FileOperations,
    /// Settings used to prepare compaction.
    pub settings: CompactionSettings,
}

/// `TreePreparation` (types.ts:868-877).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreePreparation {
    pub target_id: String,
    /// Upstream `string | null`: always serialized.
    pub old_leaf_id: Option<String>,
    /// Upstream `string | null`: always serialized.
    pub common_ancestor_id: Option<String>,
    pub entries_to_summarize: Vec<SessionEntry>,
    pub user_wants_summary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `GenerateBranchSummaryOptions` (types.ts:879-887).
#[derive(Debug, Clone)]
pub struct GenerateBranchSummaryOptions {
    pub model: Model,
    pub api_key: String,
    pub headers: Option<BTreeMap<String, String>>,
    pub signal: CancellationToken,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub reserve_tokens: Option<u64>,
}

/// `BranchSummaryResult` (types.ts:889-894).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryResult {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Options and turn state (types.ts:896-956, agent-harness.ts:153-169)
// ---------------------------------------------------------------------------

/// Callback context of [`AgentHarnessSystemPrompt::Dynamic`] (types.ts:903-909).
#[derive(Clone)]
pub struct SystemPromptContext<TContext = ()> {
    /// Upstream `session: Session` (default `Session<SessionMetadata>`).
    pub session: Arc<dyn Session<Metadata = SessionMetadata>>,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub active_tools: Vec<Arc<dyn AgentHarnessTool<TContext>>>,
    pub resources: AgentHarnessResources,
}

/// `AgentHarnessSystemPrompt` (types.ts:896-909) — static system prompt or a
/// per-turn callback producing one.
#[derive(Clone)]
pub enum AgentHarnessSystemPrompt<TContext = ()> {
    /// Upstream `string`.
    Static(String),
    /// Upstream `(context) => string | Promise<string>`; async callbacks use
    /// the boxed-future form.
    Dynamic(Arc<dyn Fn(SystemPromptContext<TContext>) -> BoxFuture<'static, String> + Send + Sync>),
}

/// `AgentHarnessOptions` (types.ts:911-956).
///
/// Upstream `TContext extends object | undefined = undefined` becomes
/// `TContext = ()`; context-free harnesses use `()` and pass
/// `tool_context: None` (upstream `toolContext: undefined`). Required fields
/// mirror upstream: `session`, `models`, `model`.
#[derive(Clone)]
pub struct AgentHarnessOptions<TContext = ()> {
    /// Upstream `session: Session` (default `Session<SessionMetadata>`).
    pub session: Arc<dyn Session<Metadata = SessionMetadata>>,
    /// Provider collection used for all model requests (turn streaming,
    /// compaction, branch summarization). Auth resolves through the
    /// providers' auth.
    pub models: Models,
    pub tools: Vec<Arc<dyn AgentHarnessTool<TContext>>>,
    /// Concrete resources available to explicit invocation methods and
    /// system-prompt callbacks. Applications own loading/reloading resources
    /// and should call `setResources()` with new values.
    pub resources: AgentHarnessResources,
    pub system_prompt: Option<AgentHarnessSystemPrompt<TContext>>,
    /// Curated stream/provider request options. Snapshotted at turn start.
    pub stream_options: Option<AgentHarnessStreamOptions>,
    /// Optional retry policy for generated compaction and branch-summary
    /// requests.
    pub retry: Option<RetryPolicy>,
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
    pub active_tool_names: Option<Vec<String>>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    /// Static context or zero-argument context provider resolved for each
    /// turn snapshot. `None` for context-free harnesses (upstream
    /// `toolContext: undefined`).
    pub tool_context: Option<AgentHarnessToolContextSource<TContext>>,
}

/// `AgentHarnessTurnState` (agent-harness.ts:153-169) — turn snapshot resolved
/// at turn start; the harness runs the loop against it so mid-turn config
/// changes do not leak into the running turn.
#[derive(Clone)]
pub struct TurnState<TContext = (), TTool = Arc<dyn AgentHarnessTool<TContext>>> {
    pub messages: Vec<AgentMessage>,
    pub resources: AgentHarnessResources,
    pub tool_context: TContext,
    pub stream_options: AgentHarnessStreamOptions,
    pub session_id: String,
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub tools: Vec<TTool>,
    pub active_tools: Vec<TTool>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rpi_ai::types::{AssistantRole, StopReason, Usage, UserRole};
    use serde::de::DeserializeOwned;
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::compaction::utils::create_file_ops;
    use crate::session::{
        CompactionEntry as SessionCompactionEntry, LeafEntry, MessageEntry, SessionEntry,
    };

    fn to_json<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).expect("serialization must succeed")
    }

    fn parse<T: DeserializeOwned>(json: &str) -> T {
        serde_json::from_str(json).expect("parse")
    }

    fn user_msg() -> AgentMessage {
        AgentMessage::User(rpi_ai::types::UserMessage {
            role: UserRole::User,
            content: rpi_ai::types::UserContent::Text("hi".to_owned()),
            timestamp: 1,
        })
    }

    fn assistant_msg() -> AgentMessage {
        AgentMessage::Assistant(rpi_ai::types::AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![],
            api: "anthropic-messages".into(),
            provider: "anthropic".to_owned(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 2,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        })
    }

    fn model() -> Model {
        parse(
            r#"{"id":"m1","name":"Test","api":"anthropic-messages","provider":"p","baseUrl":"https://x","reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":1000,"maxTokens":100}"#,
        )
    }

    fn resources() -> AgentHarnessResources {
        AgentHarnessResources::default()
    }

    fn preparation() -> CompactionPreparation {
        CompactionPreparation {
            first_kept_entry_id: "e1".to_owned(),
            messages_to_summarize: vec![user_msg()],
            turn_prefix_messages: vec![],
            retained_tail: vec![assistant_msg()],
            is_split_turn: false,
            tokens_before: 100,
            previous_summary: None,
            file_ops: create_file_ops(),
            settings: crate::compaction::DEFAULT_COMPACTION_SETTINGS,
        }
    }

    fn tree_preparation() -> TreePreparation {
        TreePreparation {
            target_id: "e2".to_owned(),
            old_leaf_id: None,
            common_ancestor_id: None,
            entries_to_summarize: vec![],
            user_wants_summary: false,
            custom_instructions: None,
            replace_instructions: None,
            label: None,
        }
    }

    /// `#[serde(skip)]` signal fields roundtrip to a fresh token; restore the
    /// original token so the roundtrip equality assertion holds (the signal is
    /// runtime-only, never on the wire).
    fn restore_signal(
        original: &AgentHarnessOwnEvent,
        mut back: AgentHarnessOwnEvent,
    ) -> AgentHarnessOwnEvent {
        match (original, &mut back) {
            (
                AgentHarnessOwnEvent::SessionBeforeCompact { signal, .. },
                AgentHarnessOwnEvent::SessionBeforeCompact {
                    signal: back_signal,
                    ..
                },
            )
            | (
                AgentHarnessOwnEvent::SessionBeforeTree { signal, .. },
                AgentHarnessOwnEvent::SessionBeforeTree {
                    signal: back_signal,
                    ..
                },
            ) => *back_signal = signal.clone(),
            _ => {}
        }
        back
    }

    fn compaction_entry() -> SessionCompactionEntry {
        SessionCompactionEntry {
            id: "c1".to_owned(),
            parent_id: None,
            timestamp: "t".to_owned(),
            summary: "s".to_owned(),
            first_kept_entry_id: None,
            tokens_before: 10,
            retained_tail: None,
            details: None,
            usage: None,
            from_hook: Some(true),
            system_message: None,
        }
    }

    #[test]
    fn error_code_literals() {
        // Upstream literal lists, byte for byte (types.ts:150-158, 176-182,
        // 197, 212, 226-232, 246-255).
        let file = [
            FileErrorCode::Aborted,
            FileErrorCode::NotFound,
            FileErrorCode::PermissionDenied,
            FileErrorCode::NotDirectory,
            FileErrorCode::IsDirectory,
            FileErrorCode::Invalid,
            FileErrorCode::NotSupported,
            FileErrorCode::Unknown,
        ];
        assert_eq!(
            file.map(FileErrorCode::as_str),
            [
                "aborted",
                "not_found",
                "permission_denied",
                "not_directory",
                "is_directory",
                "invalid",
                "not_supported",
                "unknown"
            ]
        );

        let execution = [
            ExecutionErrorCode::Aborted,
            ExecutionErrorCode::Timeout,
            ExecutionErrorCode::ShellUnavailable,
            ExecutionErrorCode::SpawnError,
            ExecutionErrorCode::CallbackError,
            ExecutionErrorCode::Unknown,
        ];
        assert_eq!(
            execution.map(ExecutionErrorCode::as_str),
            [
                "aborted",
                "timeout",
                "shell_unavailable",
                "spawn_error",
                "callback_error",
                "unknown"
            ]
        );

        let compaction = [
            CompactionErrorCode::Aborted,
            CompactionErrorCode::SummarizationFailed,
            CompactionErrorCode::InvalidSession,
            CompactionErrorCode::Unknown,
        ];
        assert_eq!(
            compaction.map(CompactionErrorCode::as_str),
            [
                "aborted",
                "summarization_failed",
                "invalid_session",
                "unknown"
            ]
        );

        // BranchSummaryErrorCode has no "unknown" upstream.
        let branch_summary = [
            BranchSummaryErrorCode::Aborted,
            BranchSummaryErrorCode::SummarizationFailed,
            BranchSummaryErrorCode::InvalidSession,
        ];
        assert_eq!(
            branch_summary.map(BranchSummaryErrorCode::as_str),
            ["aborted", "summarization_failed", "invalid_session"]
        );

        let session = [
            SessionErrorCode::NotFound,
            SessionErrorCode::InvalidSession,
            SessionErrorCode::InvalidEntry,
            SessionErrorCode::InvalidForkTarget,
            SessionErrorCode::Storage,
            SessionErrorCode::Unknown,
        ];
        assert_eq!(
            session.map(SessionErrorCode::as_str),
            [
                "not_found",
                "invalid_session",
                "invalid_entry",
                "invalid_fork_target",
                "storage",
                "unknown"
            ]
        );

        let harness = [
            AgentHarnessErrorCode::Busy,
            AgentHarnessErrorCode::InvalidState,
            AgentHarnessErrorCode::InvalidArgument,
            AgentHarnessErrorCode::Session,
            AgentHarnessErrorCode::Hook,
            AgentHarnessErrorCode::Auth,
            AgentHarnessErrorCode::Compaction,
            AgentHarnessErrorCode::BranchSummary,
            AgentHarnessErrorCode::Unknown,
        ];
        assert_eq!(
            harness.map(AgentHarnessErrorCode::as_str),
            [
                "busy",
                "invalid_state",
                "invalid_argument",
                "session",
                "hook",
                "auth",
                "compaction",
                "branch_summary",
                "unknown"
            ]
        );
    }

    #[test]
    fn harness_phase_literals() {
        let phases = [
            AgentHarnessPhase::Idle,
            AgentHarnessPhase::Turn,
            AgentHarnessPhase::Compaction,
            AgentHarnessPhase::BranchSummary,
            AgentHarnessPhase::Retry,
        ];
        assert_eq!(
            phases.map(AgentHarnessPhase::as_str),
            ["idle", "turn", "compaction", "branch_summary", "retry"]
        );
        assert_eq!(AgentHarnessPhase::default(), AgentHarnessPhase::Idle);
    }

    #[test]
    fn own_event_type_literals() {
        // All 22 variants, `type` tags as upstream harness/types.ts:561-740.
        let cases: Vec<(AgentHarnessOwnEvent, &str)> = vec![
            (
                AgentHarnessOwnEvent::QueueUpdate {
                    steer: vec![],
                    follow_up: vec![],
                    next_turn: vec![],
                },
                "queue_update",
            ),
            (
                AgentHarnessOwnEvent::SavePoint {
                    had_pending_mutations: false,
                },
                "save_point",
            ),
            (
                AgentHarnessOwnEvent::Abort {
                    cleared_steer: vec![],
                    cleared_follow_up: vec![],
                },
                "abort",
            ),
            (
                AgentHarnessOwnEvent::Settled { next_turn_count: 0 },
                "settled",
            ),
            (
                AgentHarnessOwnEvent::BeforeAgentStart {
                    prompt: "p".to_owned(),
                    images: None,
                    system_prompt: "s".to_owned(),
                    resources: resources(),
                },
                "before_agent_start",
            ),
            (
                AgentHarnessOwnEvent::Context { messages: vec![] },
                "context",
            ),
            (
                AgentHarnessOwnEvent::BeforeProviderRequest {
                    model: Box::new(model()),
                    session_id: "s1".to_owned(),
                    stream_options: AgentHarnessStreamOptions::default(),
                },
                "before_provider_request",
            ),
            (
                AgentHarnessOwnEvent::BeforeProviderPayload {
                    model: Box::new(model()),
                    payload: json!({}),
                },
                "before_provider_payload",
            ),
            (
                AgentHarnessOwnEvent::AfterProviderResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                },
                "after_provider_response",
            ),
            (
                AgentHarnessOwnEvent::ToolCall {
                    tool_call_id: "c".to_owned(),
                    tool_name: "read".to_owned(),
                    input: Map::new(),
                },
                "tool_call",
            ),
            (
                AgentHarnessOwnEvent::ToolResult {
                    tool_call_id: "c".to_owned(),
                    tool_name: "read".to_owned(),
                    input: Map::new(),
                    content: vec![],
                    details: json!({}),
                    is_error: false,
                    usage: None,
                },
                "tool_result",
            ),
            (
                AgentHarnessOwnEvent::SessionBeforeCompact {
                    preparation: preparation(),
                    branch_entries: vec![],
                    custom_instructions: None,
                    signal: CancellationToken::new(),
                },
                "session_before_compact",
            ),
            (
                AgentHarnessOwnEvent::SessionCompact {
                    compaction_entry: compaction_entry(),
                    from_hook: true,
                },
                "session_compact",
            ),
            (
                AgentHarnessOwnEvent::SessionBeforeTree {
                    preparation: tree_preparation(),
                    signal: CancellationToken::new(),
                },
                "session_before_tree",
            ),
            (
                AgentHarnessOwnEvent::SessionTree {
                    new_leaf_id: None,
                    old_leaf_id: None,
                    summary_entry: None,
                    from_hook: None,
                },
                "session_tree",
            ),
            (
                AgentHarnessOwnEvent::RetryScheduled {
                    operation: RetryOperation::Compaction,
                    attempt: 1,
                    max_attempts: 3,
                    delay_ms: 500,
                    error_message: "e".to_owned(),
                },
                "retry_scheduled",
            ),
            (
                AgentHarnessOwnEvent::RetryAttemptStart {
                    operation: RetryOperation::BranchSummary,
                },
                "retry_attempt_start",
            ),
            (
                AgentHarnessOwnEvent::RetryFinished {
                    operation: RetryOperation::Compaction,
                },
                "retry_finished",
            ),
            (
                AgentHarnessOwnEvent::ModelUpdate {
                    model: Box::new(model()),
                    previous_model: None,
                    source: UpdateSource::Set,
                },
                "model_update",
            ),
            (
                AgentHarnessOwnEvent::ThinkingLevelUpdate {
                    level: ThinkingLevel::High,
                    previous_level: ThinkingLevel::Low,
                },
                "thinking_level_update",
            ),
            (
                AgentHarnessOwnEvent::ToolsUpdate {
                    tool_names: vec![],
                    previous_tool_names: vec![],
                    active_tool_names: vec![],
                    previous_active_tool_names: vec![],
                    source: UpdateSource::Restore,
                },
                "tools_update",
            ),
            (
                AgentHarnessOwnEvent::ResourcesUpdate {
                    resources: resources(),
                    previous_resources: resources(),
                },
                "resources_update",
            ),
        ];
        assert_eq!(
            cases.len(),
            22,
            "AgentHarnessOwnEvent has exactly 22 variants"
        );
        for (event, expected_type) in &cases {
            let v: Value = parse(&to_json(event));
            assert_eq!(v["type"], json!(expected_type));
            // Roundtrip through the tagged union.
            let back: AgentHarnessOwnEvent = parse(&to_json(event));
            assert_eq!(&restore_signal(event, back), event);
        }
    }

    #[test]
    fn own_event_payload_field_names() {
        // QueueUpdateEvent: steer/followUp/nextTurn.
        let queue = AgentHarnessOwnEvent::QueueUpdate {
            steer: vec![user_msg()],
            follow_up: vec![],
            next_turn: vec![assistant_msg()],
        };
        let v: Value = parse(&to_json(&queue));
        assert_eq!(v["steer"][0]["role"], json!("user"));
        assert!(v.get("followUp").is_some());
        assert!(v.get("nextTurn").is_some());

        // ToolCallEvent: toolCallId/toolName/input.
        let tool_call = AgentHarnessOwnEvent::ToolCall {
            tool_call_id: "c1".to_owned(),
            tool_name: "read".to_owned(),
            input: serde_json::from_str(r#"{"path":"/a"}"#).expect("map"),
        };
        assert_eq!(
            to_json(&tool_call),
            r#"{"type":"tool_call","toolCallId":"c1","toolName":"read","input":{"path":"/a"}}"#
        );

        // RetryScheduledEvent: operation/attempt/maxAttempts/delayMs/errorMessage.
        let retry = AgentHarnessOwnEvent::RetryScheduled {
            operation: RetryOperation::BranchSummary,
            attempt: 2,
            max_attempts: 4,
            delay_ms: 1000,
            error_message: "boom".to_owned(),
        };
        assert_eq!(
            to_json(&retry),
            r#"{"type":"retry_scheduled","operation":"branch_summary","attempt":2,"maxAttempts":4,"delayMs":1000,"errorMessage":"boom"}"#
        );

        // ModelUpdateEvent: previousModel/source; ThinkingLevelUpdateEvent:
        // level/previousLevel (camelCase of "previous_level").
        let model_update = AgentHarnessOwnEvent::ModelUpdate {
            model: Box::new(model()),
            previous_model: Some(Box::new(model())),
            source: UpdateSource::Restore,
        };
        let v: Value = parse(&to_json(&model_update));
        assert!(v.get("previousModel").is_some());
        assert_eq!(v["source"], json!("restore"));

        let level_update = AgentHarnessOwnEvent::ThinkingLevelUpdate {
            level: ThinkingLevel::Max,
            previous_level: ThinkingLevel::Off,
        };
        let v: Value = parse(&to_json(&level_update));
        assert_eq!(v["level"], json!("max"));
        assert_eq!(v["previousLevel"], json!("off"));

        // SessionTreeEvent: newLeafId/oldLeafId; nulls for `string | null`.
        let tree = AgentHarnessOwnEvent::SessionTree {
            new_leaf_id: Some("e9".to_owned()),
            old_leaf_id: None,
            summary_entry: None,
            from_hook: None,
        };
        let v: Value = parse(&to_json(&tree));
        assert_eq!(v["newLeafId"], json!("e9"));
        assert_eq!(v["oldLeafId"], Value::Null);
        assert!(v.get("summaryEntry").is_none());
        assert!(v.get("fromHook").is_none());

        // SessionBeforeCompactEvent: preparation/branchEntries camelCase; the
        // runtime-only signal never appears on the wire.
        let compact = AgentHarnessOwnEvent::SessionBeforeCompact {
            preparation: preparation(),
            branch_entries: vec![SessionEntry::Message(MessageEntry {
                id: "e1".to_owned(),
                parent_id: None,
                timestamp: "t".to_owned(),
                message: user_msg(),
            })],
            custom_instructions: Some("ci".to_owned()),
            signal: CancellationToken::new(),
        };
        let v: Value = parse(&to_json(&compact));
        assert_eq!(v["preparation"]["firstKeptEntryId"], json!("e1"));
        assert_eq!(v["branchEntries"][0]["type"], json!("message"));
        assert_eq!(v["customInstructions"], json!("ci"));
        assert!(v.get("signal").is_none());
        let back: AgentHarnessOwnEvent = parse(&to_json(&compact));
        assert_eq!(restore_signal(&compact, back), compact);

        // BeforeAgentStartEvent: prompt/images/systemPrompt/resources, with a
        // skill serialized under resources.skills.
        let start = AgentHarnessOwnEvent::BeforeAgentStart {
            prompt: "p".to_owned(),
            images: Some(vec![ImageContent {
                data: "aGk=".to_owned(),
                mime_type: "image/png".to_owned(),
            }]),
            system_prompt: "s".to_owned(),
            resources: AgentHarnessResources {
                prompt_templates: None,
                skills: Some(vec![Skill {
                    name: "n".to_owned(),
                    description: "d".to_owned(),
                    content: "c".to_owned(),
                    file_path: "/a/SKILL.md".to_owned(),
                    disable_model_invocation: Some(true),
                }]),
            },
        };
        let v: Value = parse(&to_json(&start));
        assert_eq!(v["images"][0]["mimeType"], json!("image/png"));
        assert_eq!(v["systemPrompt"], json!("s"));
        assert_eq!(
            v["resources"]["skills"][0]["filePath"],
            json!("/a/SKILL.md")
        );
        assert_eq!(
            v["resources"]["skills"][0]["disableModelInvocation"],
            json!(true)
        );
        assert!(v["resources"].get("promptTemplates").is_none());
    }

    #[test]
    fn agent_harness_event_untagged() {
        // The union is transparent on the wire: no wrapper object.
        let agent = AgentHarnessEvent::Agent(AgentEvent::TurnStart);
        assert_eq!(to_json(&agent), r#"{"type":"turn_start"}"#);
        let back: AgentHarnessEvent = parse(&to_json(&agent));
        assert_eq!(back, agent);

        let harness = AgentHarnessEvent::Harness(AgentHarnessOwnEvent::SavePoint {
            had_pending_mutations: true,
        });
        assert_eq!(
            to_json(&harness),
            r#"{"type":"save_point","hadPendingMutations":true}"#
        );
        let back: AgentHarnessEvent = parse(&to_json(&harness));
        assert_eq!(back, harness);
    }

    #[test]
    fn pending_session_write_type_literals() {
        // All 11 entry forms minus id/parentId/timestamp (types.ts:555-559).
        let cases: Vec<(PendingSessionWrite, &str)> = vec![
            (
                PendingSessionWrite::Message {
                    message: user_msg(),
                },
                "message",
            ),
            (
                PendingSessionWrite::ThinkingLevelChange {
                    thinking_level: "high".to_owned(),
                },
                "thinking_level_change",
            ),
            (
                PendingSessionWrite::ModelChange {
                    provider: "p".to_owned(),
                    model_id: "m".to_owned(),
                },
                "model_change",
            ),
            (
                PendingSessionWrite::ActiveToolsChange {
                    active_tool_names: vec!["read".to_owned()],
                },
                "active_tools_change",
            ),
            (
                PendingSessionWrite::Compaction {
                    summary: "s".to_owned(),
                    first_kept_entry_id: None,
                    tokens_before: 5,
                    retained_tail: None,
                    details: None,
                    usage: None,
                    from_hook: None,
                },
                "compaction",
            ),
            (
                PendingSessionWrite::BranchSummary {
                    from_id: "e0".to_owned(),
                    summary: "s".to_owned(),
                    details: None,
                    usage: None,
                    from_hook: None,
                },
                "branch_summary",
            ),
            (
                PendingSessionWrite::Custom {
                    custom_type: "artifact-index".to_owned(),
                    data: None,
                },
                "custom",
            ),
            (
                PendingSessionWrite::CustomMessage {
                    custom_type: "note".to_owned(),
                    content: UserContent::Text("hello".to_owned()),
                    details: None,
                    display: true,
                },
                "custom_message",
            ),
            (
                PendingSessionWrite::Label {
                    target_id: "e1".to_owned(),
                    label: None,
                },
                "label",
            ),
            (
                PendingSessionWrite::SessionInfo { name: None },
                "session_info",
            ),
            (PendingSessionWrite::Leaf { target_id: None }, "leaf"),
        ];
        assert_eq!(cases.len(), 11, "PendingSessionWrite has 11 forms");
        for (write, expected_type) in &cases {
            let v: Value = parse(&to_json(write));
            assert_eq!(v["type"], json!(expected_type));
            let back: PendingSessionWrite = parse(&to_json(write));
            assert_eq!(&back, write);
        }

        // Field names and nullability follow the persisted entry wire shapes.
        let thinking = PendingSessionWrite::ThinkingLevelChange {
            thinking_level: "max".to_owned(),
        };
        assert_eq!(
            to_json(&thinking),
            r#"{"type":"thinking_level_change","thinkingLevel":"max"}"#
        );
        let tools = PendingSessionWrite::ActiveToolsChange {
            active_tool_names: vec!["bash".to_owned()],
        };
        assert_eq!(
            to_json(&tools),
            r#"{"type":"active_tools_change","activeToolNames":["bash"]}"#
        );
        let compaction = PendingSessionWrite::Compaction {
            summary: "s".to_owned(),
            first_kept_entry_id: Some("e9".to_owned()),
            tokens_before: 100,
            retained_tail: None,
            details: None,
            usage: None,
            from_hook: None,
        };
        assert_eq!(
            to_json(&compaction),
            r#"{"type":"compaction","summary":"s","firstKeptEntryId":"e9","tokensBefore":100}"#
        );
        // Leaf.targetId is `string | null` upstream: always serialized.
        let leaf = PendingSessionWrite::Leaf { target_id: None };
        assert_eq!(to_json(&leaf), r#"{"type":"leaf","targetId":null}"#);
        // Label.label is `string | undefined`: omitted when None.
        let label = PendingSessionWrite::Label {
            target_id: "e1".to_owned(),
            label: None,
        };
        assert_eq!(to_json(&label), r#"{"type":"label","targetId":"e1"}"#);
    }

    #[test]
    fn stream_options_serde_shape() {
        let options = AgentHarnessStreamOptions {
            transport: Some(Transport::Websocket),
            timeout_ms: Some(30_000),
            max_retries: Some(2),
            max_retry_delay_ms: Some(5_000),
            headers: Some(BTreeMap::from([("x-a".to_owned(), "1".to_owned())])),
            metadata: Some(BTreeMap::from([("k".to_owned(), json!(1))])),
            cache_retention: Some(CacheRetention::Short),
        };
        assert_eq!(
            to_json(&options),
            r#"{"transport":"websocket","timeoutMs":30000,"maxRetries":2,"maxRetryDelayMs":5000,"headers":{"x-a":"1"},"metadata":{"k":1},"cacheRetention":"short"}"#
        );
        let back: AgentHarnessStreamOptions = parse(&to_json(&options));
        assert_eq!(back, options);

        // Patch with a header deletion: `null` on the wire, `None` in memory.
        let patch = AgentHarnessStreamOptionsPatch {
            transport: None,
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: None,
            cache_retention: None,
            headers: PatchMap::Merge(BTreeMap::from([("x-a".to_owned(), None)])),
            metadata: PatchMap::Absent,
        };
        assert_eq!(
            to_json(&patch),
            r#"{"headers":{"x-a":null}}"#,
            "absent fields are omitted; deletions serialize as null"
        );
        let back: AgentHarnessStreamOptionsPatch = parse(&to_json(&patch));
        assert_eq!(back, patch);
    }

    #[test]
    fn apply_stream_options_patch_semantics() {
        let base = AgentHarnessStreamOptions {
            transport: Some(Transport::Auto),
            timeout_ms: Some(10_000),
            max_retries: None,
            max_retry_delay_ms: None,
            headers: Some(BTreeMap::from([
                ("keep".to_owned(), "1".to_owned()),
                ("drop".to_owned(), "2".to_owned()),
            ])),
            metadata: Some(BTreeMap::from([("a".to_owned(), json!(1))])),
            cache_retention: None,
        };

        // No patch: an untouched clone.
        assert_eq!(
            apply_stream_options_patch(&base, None),
            base,
            "no patch returns a clone of the base"
        );

        // Scalar replacement only when present in the patch.
        let patched = apply_stream_options_patch(
            &base,
            Some(&AgentHarnessStreamOptionsPatch {
                transport: Some(Transport::Sse),
                timeout_ms: None,
                max_retries: Some(3),
                max_retry_delay_ms: None,
                cache_retention: Some(CacheRetention::Long),
                headers: PatchMap::Absent,
                metadata: PatchMap::Absent,
            }),
        );
        assert_eq!(patched.transport, Some(Transport::Sse));
        assert_eq!(patched.timeout_ms, Some(10_000), "absent scalar is kept");
        assert_eq!(patched.max_retries, Some(3));
        assert_eq!(patched.headers, base.headers, "absent header patch is kept");

        // Header merge: set + delete; empty result collapses to None.
        let patched = apply_stream_options_patch(
            &base,
            Some(&AgentHarnessStreamOptionsPatch {
                headers: PatchMap::Merge(BTreeMap::from([
                    ("drop".to_owned(), None),
                    ("add".to_owned(), Some("3".to_owned())),
                ])),
                ..AgentHarnessStreamOptionsPatch::default()
            }),
        );
        assert_eq!(
            patched.headers,
            Some(BTreeMap::from([
                ("keep".to_owned(), "1".to_owned()),
                ("add".to_owned(), "3".to_owned()),
            ]))
        );

        // Deleting every base header yields None (upstream empty-map check).
        let patched = apply_stream_options_patch(
            &base,
            Some(&AgentHarnessStreamOptionsPatch {
                headers: PatchMap::Merge(BTreeMap::from([
                    ("keep".to_owned(), None),
                    ("drop".to_owned(), None),
                ])),
                ..AgentHarnessStreamOptionsPatch::default()
            }),
        );
        assert_eq!(patched.headers, None);

        // Explicit `undefined` (PatchMap::Clear) clears the whole header set.
        let patched = apply_stream_options_patch(
            &base,
            Some(&AgentHarnessStreamOptionsPatch {
                headers: PatchMap::Clear,
                ..AgentHarnessStreamOptionsPatch::default()
            }),
        );
        assert_eq!(patched.headers, None);
        assert_eq!(patched.metadata, base.metadata);

        // Metadata merge mirrors headers.
        let patched = apply_stream_options_patch(
            &base,
            Some(&AgentHarnessStreamOptionsPatch {
                metadata: PatchMap::Merge(BTreeMap::from([
                    ("a".to_owned(), None),
                    ("b".to_owned(), Some(json!(2))),
                ])),
                ..AgentHarnessStreamOptionsPatch::default()
            }),
        );
        assert_eq!(
            patched.metadata,
            Some(BTreeMap::from([("b".to_owned(), json!(2))]))
        );
    }

    #[test]
    fn session_metadata_serde_shape() {
        // JsonlSessionMetadata flattens the inherited SessionMetadata: flat
        // camelCase object, optional keys omitted.
        let metadata = JsonlSessionMetadata {
            base: SessionMetadata {
                id: "s1".to_owned(),
                created_at: "2026-07-30T00:00:00.000Z".to_owned(),
            },
            cwd: "/repo".to_owned(),
            path: "/sessions/s1.jsonl".to_owned(),
            parent_session_path: Some("/sessions/s0.jsonl".to_owned()),
            metadata: None,
        };
        assert_eq!(
            to_json(&metadata),
            r#"{"id":"s1","createdAt":"2026-07-30T00:00:00.000Z","cwd":"/repo","path":"/sessions/s1.jsonl","parentSessionPath":"/sessions/s0.jsonl"}"#
        );
        let back: JsonlSessionMetadata = parse(&to_json(&metadata));
        assert_eq!(back, metadata);

        // SessionStats: camelCase numbers.
        let stats = SessionStats {
            message_count: 3,
            cached_tokens: 10,
            uncached_tokens: 20,
            total_tokens: 30,
            cost_total: 0.25,
        };
        assert_eq!(
            to_json(&stats),
            r#"{"messageCount":3,"cachedTokens":10,"uncachedTokens":20,"totalTokens":30,"costTotal":0.25}"#
        );
    }

    #[test]
    fn fork_position_literals() {
        assert_eq!(to_json(&ForkPosition::Before), "\"before\"");
        assert_eq!(to_json(&ForkPosition::At), "\"at\"");
    }

    #[test]
    fn hook_result_shapes() {
        // Result types serialize camelCase with `undefined` fields omitted.
        let before_start = BeforeAgentStartResult {
            messages: None,
            system_prompt: Some("sp".to_owned()),
        };
        assert_eq!(to_json(&before_start), r#"{"systemPrompt":"sp"}"#);

        let tool_result = ToolResultPatch {
            content: None,
            details: None,
            is_error: Some(true),
            usage: None,
            terminate: Some(false),
        };
        assert_eq!(
            to_json(&tool_result),
            r#"{"isError":true,"terminate":false}"#
        );

        let before_tree = SessionBeforeTreeResult {
            cancel: None,
            summary: Some(TreeSummary {
                summary: "s".to_owned(),
                details: None,
                usage: None,
            }),
            custom_instructions: Some("ci".to_owned()),
            replace_instructions: None,
            label: None,
        };
        assert_eq!(
            to_json(&before_tree),
            r#"{"summary":{"summary":"s"},"customInstructions":"ci"}"#
        );

        // The no-result events map to the default variant.
        assert_eq!(HarnessHookResult::default(), HarnessHookResult::NoResult);
        // Explicit undefined (`None`) for a result-bearing event is distinct.
        assert!(matches!(
            HarnessHookResult::BeforeAgentStart(None),
            HarnessHookResult::BeforeAgentStart(None)
        ));
    }

    #[test]
    fn session_entry_roundtrip_in_events() {
        // The harness union is the shared crate session union; a leaf entry
        // inside a session_tree event roundtrips intact.
        let event = AgentHarnessOwnEvent::SessionTree {
            new_leaf_id: Some("l1".to_owned()),
            old_leaf_id: None,
            summary_entry: None,
            from_hook: None,
        };
        let _ = LeafEntry {
            id: "l1".to_owned(),
            parent_id: Some("e1".to_owned()),
            timestamp: "t".to_owned(),
            target_id: None,
        };
        let back: AgentHarnessOwnEvent = parse(&to_json(&event));
        assert_eq!(back, event);
    }
}
