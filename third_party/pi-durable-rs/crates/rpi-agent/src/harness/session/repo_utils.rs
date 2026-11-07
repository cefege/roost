//! Port of `packages/agent/src/harness/session/repo-utils.ts` @ pi 0.82.1 (2efa728) —
//! helpers shared by the session storage and repository implementations.
//!
//! Intentional differences:
//! - `getFileSystemResultOrThrow` (repo-utils.ts:24-30) returns `Result<T, SessionError>`
//!   — Rust has no exceptions; the upstream `cause` chain is dropped (types.rs header
//!   note: wrappers copy the cause text into the message).
//! - `getEntriesToFork` (repo-utils.ts:32-50) takes `entry_id: Option<&str>` and
//!   `position: Option<ForkPosition>` instead of a `{entryId?, position?}` options
//!   object (Rust has no optional-property structs; `ForkPosition` is
//!   `harness/types.rs:1163`).
//! - `toSession` (repo-utils.ts:20-22) wraps the storage in the [`SessionFacade`]
//!   struct (`session_facade.rs`, port of session.ts:150) with default
//!   context-build options, upcast to the `types::Session` trait object.
//! - `updateLabelCache` / `buildLabelsById` / `leafIdAfterEntry` / `generateEntryId`
//!   are duplicated verbatim between jsonl-storage.ts:25-51 and memory-storage.ts:11-37
//!   upstream; they are shared here to avoid the duplication.
//! - `createTimestamp` uses the `new Date(ms).toISOString()` equivalent already ported
//!   by the T07 main path (session_manager.rs:62-105); that helper is private to the
//!   `rpi` crate, so the same algorithm is re-implemented here.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rpi_ai::utils::uuid::uuidv7_now;

use crate::harness::session::session_facade::Session as SessionFacade;
use crate::harness::types::{
    FileError, FileErrorCode, ForkPosition, Session, SessionContextBuildOptions,
    SessionEntryCursorOptions, SessionError, SessionErrorCode, SessionStorage,
};
use crate::messages::AgentMessage;
use crate::session::{MessageEntry, SessionEntry};

// ---------------------------------------------------------------------------
// Time helpers (`Date.now()` / `new Date().toISOString()` equivalents —
// same algorithm as session_manager.rs:62-105, private to the `rpi` crate)
// ---------------------------------------------------------------------------

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Civil date from days since epoch (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// `new Date(ms).toISOString()` — `YYYY-MM-DDTHH:MM:SS.sssZ`.
pub(crate) fn format_iso8601_ms(ms: u64) -> String {
    let ms = ms as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, mo, d) = civil_from_days(days);
    let h = rem / 3_600_000;
    let mi = (rem % 3_600_000) / 60_000;
    let s = (rem % 60_000) / 1000;
    let milli = rem % 1000;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// `new Date().toISOString()` — ISO-8601 UTC timestamp with milliseconds.
pub(crate) fn now_iso8601() -> String {
    format_iso8601_ms(now_ms())
}

// ---------------------------------------------------------------------------
// repo-utils.ts
// ---------------------------------------------------------------------------

/// `createSessionId` (repo-utils.ts:12-14).
pub(crate) fn create_session_id() -> String {
    uuidv7_now()
}

/// `createTimestamp` (repo-utils.ts:16-18).
pub(crate) fn create_timestamp() -> String {
    now_iso8601()
}

/// `getFileSystemResultOrThrow` (repo-utils.ts:24-30) — map a [`FileError`] to a
/// [`SessionError`]: `not_found` stays `not_found`, everything else becomes `storage`.
pub(crate) fn get_file_system_result_or_throw<T>(
    result: Result<T, FileError>,
    message: impl Into<String>,
) -> Result<T, SessionError> {
    result.map_err(|error| {
        let code = if error.code == FileErrorCode::NotFound {
            SessionErrorCode::NotFound
        } else {
            SessionErrorCode::Storage
        };
        SessionError::new(code, format!("{}: {}", message.into(), error.message))
    })
}

/// `generateEntryId` (jsonl-storage.ts:43-51) — short id from the uuidv7 tail.
///
/// The uuidv7 prefix is timestamp-derived and nearly constant between calls, so short
/// ids must come from the random tail; falls back to a full uuidv7 after 100 collisions.
pub(crate) fn generate_entry_id(by_id: &HashMap<String, SessionEntry>) -> String {
    for _ in 0..100 {
        let id = uuidv7_now();
        let short = &id[id.len().saturating_sub(8)..];
        if !by_id.contains_key(short) {
            return short.to_owned();
        }
    }
    uuidv7_now()
}

/// `updateLabelCache` (jsonl-storage.ts:25-33) — label entries maintain a
/// `targetId -> trimmed label` cache; empty/absent labels delete the entry.
pub(crate) fn update_label_cache(labels_by_id: &mut HashMap<String, String>, entry: &SessionEntry) {
    let SessionEntry::Label(label) = entry else {
        return;
    };
    match label.label.as_deref().map(str::trim) {
        Some(trimmed) if !trimmed.is_empty() => {
            labels_by_id.insert(label.target_id.clone(), trimmed.to_owned());
        }
        _ => {
            labels_by_id.remove(&label.target_id);
        }
    }
}

/// `buildLabelsById` (jsonl-storage.ts:35-41).
pub(crate) fn build_labels_by_id(entries: &[SessionEntry]) -> HashMap<String, String> {
    let mut labels_by_id = HashMap::new();
    for entry in entries {
        update_label_cache(&mut labels_by_id, entry);
    }
    labels_by_id
}

/// `leafIdAfterEntry` (jsonl-storage.ts:134-136) — leaf entries move the leaf to their
/// `targetId`; every other entry becomes the leaf.
pub(crate) fn leaf_id_after_entry(entry: &SessionEntry) -> Option<String> {
    match entry {
        SessionEntry::Leaf(leaf) => leaf.target_id.clone(),
        other => Some(other.id().to_owned()),
    }
}

/// `toSession` (repo-utils.ts:20-22) — wraps the storage in the [`SessionFacade`]
/// struct (session.ts:150) with default context-build options, upcast to
/// the `types::Session` trait object.
pub(crate) fn to_session<TMetadata: Send + Sync + 'static>(
    storage: Arc<dyn SessionStorage<Metadata = TMetadata>>,
) -> Arc<dyn Session<Metadata = TMetadata>> {
    Arc::new(SessionFacade::new(
        storage,
        SessionContextBuildOptions::default(),
    ))
}

/// `getEntriesToFork` (repo-utils.ts:32-50) — entries copied into a fork:
/// - no `entry_id`: the full entry list;
/// - `position: "at"`: the path ending at the target entry (included);
/// - `position: "before"` (default): the target must be a user message, and the path
///   ends at its parent (the message itself is not copied).
pub(crate) async fn get_entries_to_fork<TMetadata: Send + Sync + 'static>(
    storage: &dyn SessionStorage<Metadata = TMetadata>,
    entry_id: Option<&str>,
    position: Option<ForkPosition>,
) -> Result<Vec<SessionEntry>, SessionError> {
    let Some(entry_id) = entry_id else {
        return storage
            .get_entries(SessionEntryCursorOptions::default())
            .await;
    };
    let Some(target) = storage.get_entry(entry_id).await? else {
        return Err(SessionError::new(
            SessionErrorCode::InvalidForkTarget,
            format!("Entry {entry_id} not found"),
        ));
    };
    let effective_leaf_id = match position.unwrap_or(ForkPosition::Before) {
        ForkPosition::At => Some(target.id().to_owned()),
        ForkPosition::Before => {
            let is_user_message = matches!(
                &target,
                SessionEntry::Message(MessageEntry {
                    message: AgentMessage::User(_),
                    ..
                })
            );
            if !is_user_message {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidForkTarget,
                    format!("Entry {entry_id} is not a user message"),
                ));
            }
            target.parent_id().map(str::to_owned)
        }
    };
    storage
        .get_path_to_root_or_compaction(effective_leaf_id.as_deref())
        .await
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::UNIX_EPOCH;

    use async_trait::async_trait;
    use rpi_ai::types::{
        ApiKind, AssistantContent, AssistantMessage, AssistantRole, StopReason, TextContent, Usage,
        UsageCost, UserContent, UserMessage, UserRole,
    };
    use tokio_util::sync::CancellationToken;

    use crate::harness::types::{
        CreateDirOptions, CreateTempFileOptions, FileError, FileErrorCode, FileInfo, FileKind,
        FileSystem, ReadTextLinesOptions, RemoveOptions,
    };
    use crate::messages::AgentMessage;
    use crate::session::{MessageEntry, SessionEntry};

    /// Temp-dir-backed [`FileSystem`] for storage/repo tests — the upstream tests run
    /// against `NodeExecutionEnv` (test/harness/storage.test.ts:4); the Rust
    /// `NodeExecutionEnv` port lives in `harness/env/nodejs.rs` (parallel wave), so
    /// tests use this stand-in. Blocking I/O is fine here (test-only code).
    pub(crate) struct TestFs {
        root: PathBuf,
        cwd: String,
        deny_read_text_file: bool,
        last_max_lines: Mutex<Option<usize>>,
    }

    fn unique_suffix() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    impl TestFs {
        fn new_inner(deny_read_text_file: bool) -> Arc<Self> {
            let root =
                std::env::temp_dir().join(format!("rpi-agent-session-test-{}", unique_suffix()));
            std::fs::create_dir_all(&root).expect("create test temp dir");
            Arc::new(Self {
                cwd: root.to_string_lossy().into_owned(),
                root,
                deny_read_text_file,
                last_max_lines: Mutex::new(None),
            })
        }

        pub(crate) fn new() -> Arc<Self> {
            Self::new_inner(false)
        }

        /// Like [`TestFs::new`] but `read_text_file` always fails — proves that
        /// metadata loading goes through `read_text_lines` (storage.test.ts:475-503).
        pub(crate) fn new_denying_read_text_file() -> Arc<Self> {
            Self::new_inner(true)
        }

        pub(crate) fn root(&self) -> &Path {
            &self.root
        }

        /// Last `maxLines` passed to `read_text_lines`, if any.
        pub(crate) fn last_max_lines(&self) -> Option<usize> {
            *self
                .last_max_lines
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        }
    }

    impl Drop for TestFs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn file_kind(file_type: &std::fs::FileType) -> FileKind {
        if file_type.is_symlink() {
            FileKind::Symlink
        } else if file_type.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
        }
    }

    fn io_error(path: &str, operation: &str, error: io::Error) -> FileError {
        let code = if error.kind() == io::ErrorKind::NotFound {
            FileErrorCode::NotFound
        } else {
            FileErrorCode::Unknown
        };
        FileError::new(code, format!("{operation} {path}: {error}"))
    }

    #[async_trait]
    impl FileSystem for TestFs {
        fn cwd(&self) -> &str {
            &self.cwd
        }

        async fn absolute_path(
            &self,
            path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            let p = Path::new(path);
            Ok(if p.is_absolute() {
                path.to_owned()
            } else {
                self.root.join(p).to_string_lossy().into_owned()
            })
        }

        async fn join_path(
            &self,
            parts: &[String],
            _abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            Ok(parts.join("/"))
        }

        async fn read_text_file(
            &self,
            path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            if self.deny_read_text_file {
                return Err(FileError::new(
                    FileErrorCode::NotFound,
                    format!("readTextFile should not be called for metadata ({path})"),
                ));
            }
            std::fs::read_to_string(path).map_err(|error| io_error(path, "read", error))
        }

        async fn read_text_lines(
            &self,
            path: &str,
            options: ReadTextLinesOptions,
        ) -> Result<Vec<String>, FileError> {
            // Read directly from disk, not via `read_text_file`: the metadata-load
            // test denies `read_text_file` to prove `read_text_lines` is used.
            let content =
                std::fs::read_to_string(path).map_err(|error| io_error(path, "read", error))?;
            *self
                .last_max_lines
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = options.max_lines;
            let mut lines: Vec<String> = content.split('\n').map(str::to_owned).collect();
            if let Some(max) = options.max_lines {
                lines.truncate(max);
            }
            Ok(lines)
        }

        async fn read_binary_file(
            &self,
            path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<Vec<u8>, FileError> {
            std::fs::read(path).map_err(|error| io_error(path, "read", error))
        }

        async fn write_file(
            &self,
            path: &str,
            content: &[u8],
            _abort_signal: Option<CancellationToken>,
        ) -> Result<(), FileError> {
            let p = Path::new(path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|error| io_error(path, "mkdir", error))?;
            }
            std::fs::write(p, content).map_err(|error| io_error(path, "write", error))
        }

        async fn append_file(
            &self,
            path: &str,
            content: &[u8],
            _abort_signal: Option<CancellationToken>,
        ) -> Result<(), FileError> {
            use std::io::Write;
            let p = Path::new(path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|error| io_error(path, "mkdir", error))?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .map_err(|error| io_error(path, "open", error))?;
            file.write_all(content)
                .map_err(|error| io_error(path, "append", error))
        }

        async fn rename_file(
            &self,
            source_path: &str,
            destination_path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<(), FileError> {
            std::fs::rename(source_path, destination_path)
                .map_err(|error| io_error(source_path, "rename", error))
        }

        async fn file_info(
            &self,
            path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<FileInfo, FileError> {
            let p = Path::new(path);
            let metadata =
                std::fs::symlink_metadata(p).map_err(|error| io_error(path, "stat", error))?;
            Ok(FileInfo {
                name: p
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: path.to_owned(),
                kind: file_kind(&metadata.file_type()),
                size: metadata.len(),
                mtime_ms: metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as f64)
                    .unwrap_or(0.0),
            })
        }

        async fn list_dir(
            &self,
            path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<Vec<FileInfo>, FileError> {
            let mut infos = Vec::new();
            for entry in std::fs::read_dir(path).map_err(|error| io_error(path, "list", error))? {
                let entry = entry.map_err(|error| io_error(path, "list", error))?;
                let path = entry.path().to_string_lossy().into_owned();
                infos.push(self.file_info(&path, None).await?);
            }
            Ok(infos)
        }

        async fn canonical_path(
            &self,
            path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|error| io_error(path, "canonicalize", error))
        }

        async fn exists(
            &self,
            path: &str,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<bool, FileError> {
            Path::new(path)
                .try_exists()
                .map_err(|error| io_error(path, "check", error))
        }

        async fn create_dir(&self, path: &str, options: CreateDirOptions) -> Result<(), FileError> {
            if options.recursive.unwrap_or(true) {
                std::fs::create_dir_all(path).map_err(|error| io_error(path, "mkdir", error))
            } else {
                std::fs::create_dir(path).map_err(|error| io_error(path, "mkdir", error))
            }
        }

        async fn remove(&self, path: &str, options: RemoveOptions) -> Result<(), FileError> {
            let p = Path::new(path);
            let result = if p.is_dir() {
                if options.recursive {
                    std::fs::remove_dir_all(p)
                } else {
                    std::fs::remove_dir(p)
                }
            } else {
                std::fs::remove_file(p)
            };
            match result {
                Ok(()) => Ok(()),
                Err(error) if options.force && error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(io_error(path, "remove", error)),
            }
        }

        async fn create_temp_dir(
            &self,
            prefix: Option<&str>,
            _abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            let dir = std::env::temp_dir().join(format!(
                "{}{}",
                prefix.unwrap_or("tmp-"),
                unique_suffix()
            ));
            std::fs::create_dir_all(&dir)
                .map_err(|error| io_error(dir.to_string_lossy().as_ref(), "mkdir", error))?;
            Ok(dir.to_string_lossy().into_owned())
        }

        async fn create_temp_file(
            &self,
            options: CreateTempFileOptions,
        ) -> Result<String, FileError> {
            let path = std::env::temp_dir().join(format!(
                "{}{}{}",
                options.prefix.as_deref().unwrap_or(""),
                unique_suffix(),
                options.suffix.as_deref().unwrap_or(""),
            ));
            std::fs::write(&path, [])
                .map_err(|error| io_error(path.to_string_lossy().as_ref(), "write", error))?;
            Ok(path.to_string_lossy().into_owned())
        }

        async fn cleanup(&self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// `createUserMessage` (test/harness/session-test-utils.ts:7-11).
    pub(crate) fn user_message(text: &str) -> AgentMessage {
        AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_owned()),
            timestamp: 0,
        })
    }

    /// `createAssistantMessage` (test/harness/session-test-utils.ts:13-29) — zeroed
    /// usage, `stopReason: "stop"`.
    pub(crate) fn assistant_message(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::Text(TextContent {
                text: text.to_owned(),
                text_signature: None,
            })],
            api: ApiKind::ANTHROPIC_MESSAGES.into(),
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        })
    }

    /// Assistant message with a custom usage payload (stats tests).
    pub(crate) fn assistant_message_with_usage(text: &str, usage: Usage) -> AgentMessage {
        let AgentMessage::Assistant(mut message) = assistant_message(text) else {
            unreachable!("assistant_message returns an Assistant message")
        };
        message.usage = usage;
        AgentMessage::Assistant(message)
    }

    /// Message entry wrapper — upstream tests spell entries inline; this is the
    /// `{type: "message", id, parentId, timestamp, message}` shape.
    pub(crate) fn message_entry(
        id: &str,
        parent_id: Option<&str>,
        timestamp: &str,
        message: AgentMessage,
    ) -> SessionEntry {
        SessionEntry::Message(MessageEntry {
            id: id.to_owned(),
            parent_id: parent_id.map(str::to_owned),
            timestamp: timestamp.to_owned(),
            message,
        })
    }

    /// Usage helper for stats tests — `getSessionStats` reads only `cost.total`
    /// (jsonl-storage.ts:326-335), so the cost components are zeroed.
    pub(crate) fn usage(
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        total_tokens: u64,
        cost_total: f64,
    ) -> Usage {
        Usage {
            input,
            output,
            cache_read,
            cache_write,
            cache_write1h: None,
            reasoning: None,
            total_tokens,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: cost_total,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::session::memory_storage::{
        InMemorySessionStorage, InMemorySessionStorageOptions,
    };
    use crate::harness::session::repo_utils::test_support::{
        assistant_message, message_entry, user_message,
    };
    use crate::harness::types::ForkPosition;

    /// `expect_err` without a `T: Debug` bound (the storage types are not `Debug`).
    fn expect_err<T, E>(result: Result<T, E>, message: &str) -> E {
        match result {
            Ok(_) => panic!("{message}: expected an error"),
            Err(error) => error,
        }
    }
    use super::*;

    fn storage_with(entries: Vec<SessionEntry>) -> InMemorySessionStorage {
        InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: Some(entries),
            metadata: None,
        })
        .expect("storage")
    }

    fn chain_entries() -> Vec<SessionEntry> {
        vec![
            message_entry(
                "user1",
                None,
                "2026-01-01T00:00:00.000Z",
                user_message("one"),
            ),
            message_entry(
                "assistant1",
                Some("user1"),
                "2026-01-01T00:00:01.000Z",
                assistant_message("two"),
            ),
            message_entry(
                "user2",
                Some("assistant1"),
                "2026-01-01T00:00:02.000Z",
                user_message("three"),
            ),
        ]
    }

    fn ids(entries: Vec<SessionEntry>) -> Vec<String> {
        entries.iter().map(|entry| entry.id().to_owned()).collect()
    }

    #[tokio::test]
    async fn test_get_entries_to_fork_copies_all_entries_without_entry_id() {
        let storage = storage_with(chain_entries());
        let entries = get_entries_to_fork(&storage, None, None)
            .await
            .expect("entries");
        assert_eq!(ids(entries), ["user1", "assistant1", "user2"]);
    }

    #[tokio::test]
    async fn test_get_entries_to_fork_before_user_message_truncates_at_parent() {
        let storage = storage_with(chain_entries());
        let entries = get_entries_to_fork(&storage, Some("user2"), None)
            .await
            .expect("entries");
        assert_eq!(ids(entries), ["user1", "assistant1"]);
    }

    #[tokio::test]
    async fn test_get_entries_to_fork_at_includes_the_target() {
        let storage = storage_with(chain_entries());
        let entries = get_entries_to_fork(&storage, Some("user2"), Some(ForkPosition::At))
            .await
            .expect("entries");
        assert_eq!(ids(entries), ["user1", "assistant1", "user2"]);
    }

    #[tokio::test]
    async fn test_get_entries_to_fork_before_root_user_message_returns_empty() {
        let storage = storage_with(chain_entries());
        // Fork before the first user message: the effective leaf is its null parentId.
        let entries = get_entries_to_fork(&storage, Some("user1"), None)
            .await
            .expect("entries");
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_get_entries_to_fork_rejects_missing_entry() {
        let storage = storage_with(chain_entries());
        let error = expect_err(
            get_entries_to_fork(&storage, Some("missing"), None).await,
            "error",
        );
        assert_eq!(error.code, SessionErrorCode::InvalidForkTarget);
        assert!(error.message.contains("Entry missing not found"));
    }

    #[tokio::test]
    async fn test_get_entries_to_fork_rejects_non_user_message_target() {
        let storage = storage_with(chain_entries());
        let error = expect_err(
            get_entries_to_fork(&storage, Some("assistant1"), None).await,
            "error",
        );
        assert_eq!(error.code, SessionErrorCode::InvalidForkTarget);
        assert!(
            error
                .message
                .contains("Entry assistant1 is not a user message")
        );
    }
}
