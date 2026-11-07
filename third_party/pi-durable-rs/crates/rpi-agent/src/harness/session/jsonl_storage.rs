//! Port of `packages/agent/src/harness/session/jsonl-storage.ts` @ pi 0.82.1 (2efa728) —
//! JSONL-backed [`SessionStorage`] implementation.
//!
//! Format (pinned upstream, coding-standards §9.1): one JSON object per line; the
//! first line is the session header (`{type: "session", version: 3, ...}`), every
//! following line is a session entry. There is no v1/v2 migration — `version !== 3`
//! is an `invalid_session` error (jsonl-storage.ts:77).
//!
//! Intentional differences:
//! - `JsonlSessionStorageFileSystem` (jsonl-storage.ts:13 — a 4-method `Pick`) is the
//!   full `FileSystem` trait behind an `Arc`; the storage only calls the four upstream
//!   methods (`readTextFile` / `readTextLines` / `writeFile` / `appendFile`).
//! - `parseHeaderLine` / `parseEntryLine` validate the raw JSON object first with the
//!   upstream messages, then deserialize into the typed entry union. Entries whose
//!   payload does not match the crate `SessionEntry` union (unknown `type` tags,
//!   missing payload fields) are rejected with `invalid_entry` where upstream would
//!   carry them as opaque casts — `session.rs` is the single source of truth, and the
//!   upstream numeric guards in `getSessionStats` (jsonl-storage.ts:326-335) are
//!   structurally unreachable for typed data.
//! - All I/O goes through the injected [`FileSystem`] (async); blocking I/O lives in
//!   the filesystem implementation (`harness/env/nodejs.rs`, parallel wave), matching
//!   the crate convention (session_manager.rs:22).
//! - `SessionHeader` (jsonl-storage.ts:15-23) is [`SessionHeaderLine`] here — it
//!   carries the `type` literal, which the crate `SessionHeader` (session.rs:228)
//!   does not.
//! - The mutable state sits behind a `tokio::sync::Mutex` because the [`Session`](super::session_facade::Session)
//!   facade calls the storage through `Arc<dyn SessionStorage>` and the write methods
//!   are `&self` (types.rs `SessionStorage` note; session.ts `getStorage`). Write
//!   methods hold the guard across the file append so that concurrent appends write
//!   lines and update memory in the same order (upstream is single-threaded) — same
//!   pattern as `AgentSession::compact` holding a tokio mutex across `.await`
//!   (agent_session.rs:1874). `getSessionName` / `getSessionStats` scan `entries`
//!   directly under the lock instead of calling `findEntries` (a tokio mutex is not
//!   reentrant).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::harness::types::{
    FileSystem, JsonlSessionMetadata, ReadTextLinesOptions, SessionEntryCursorOptions,
    SessionError, SessionErrorCode, SessionMetadata, SessionStats, SessionStorage,
};
use crate::messages::AgentMessage;
use crate::session::{CURRENT_SESSION_VERSION, LeafEntry, MessageEntry, SessionEntry};

use super::repo_utils::{
    build_labels_by_id, generate_entry_id, get_file_system_result_or_throw, leaf_id_after_entry,
    now_iso8601, update_label_cache,
};

/// `SessionHeader` (jsonl-storage.ts:15-23) — the serialized first line of a JSONL
/// session file. Field order matches the upstream `JSON.stringify` insertion order
/// (type, version, id, timestamp, cwd, parentSession, metadata).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionHeaderLine {
    r#type: String,
    version: u32,
    id: String,
    timestamp: String,
    cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
}

/// Options for [`JsonlSessionStorage::create`] — upstream `{cwd, sessionId,
/// parentSessionPath?, metadata?}` (jsonl-storage.ts:217-225).
#[derive(Debug, Clone)]
pub struct JsonlSessionStorageCreateOptions {
    pub cwd: String,
    pub session_id: String,
    pub parent_session_path: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

/// `invalidSession` (jsonl-storage.ts:53-55).
fn invalid_session(file_path: &str, message: &str) -> SessionError {
    SessionError::new(
        SessionErrorCode::InvalidSession,
        format!("Invalid JSONL session file {file_path}: {message}"),
    )
}

/// `invalidEntry` (jsonl-storage.ts:57-63).
fn invalid_entry(file_path: &str, line_number: usize, message: &str) -> SessionError {
    SessionError::new(
        SessionErrorCode::InvalidEntry,
        format!("Invalid JSONL session file {file_path}: line {line_number} {message}"),
    )
}

/// `parseHeaderLine` (jsonl-storage.ts:65-101) — strict validation with the upstream
/// error messages, in the upstream order.
fn parse_header_line(line: &str, file_path: &str) -> Result<SessionHeaderLine, SessionError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|_| invalid_session(file_path, "first line is not a valid session header"))?;
    let Some(object) = value.as_object() else {
        return Err(invalid_session(
            file_path,
            "first line is not a valid session header",
        ));
    };
    if object.get("type").and_then(Value::as_str) != Some("session") {
        return Err(invalid_session(
            file_path,
            "first line is not a valid session header",
        ));
    }
    // `header.version !== 3` — number comparison (jsonl-storage.ts:77); a JSON `3.0`
    // compares equal in JS, hence `as_f64`.
    if object.get("version").and_then(Value::as_f64) != Some(CURRENT_SESSION_VERSION as f64) {
        return Err(invalid_session(file_path, "unsupported session version"));
    }
    match object.get("id") {
        Some(Value::String(id)) if !id.is_empty() => {}
        _ => return Err(invalid_session(file_path, "session header is missing id")),
    }
    match object.get("timestamp") {
        Some(Value::String(timestamp)) if !timestamp.is_empty() => {}
        _ => {
            return Err(invalid_session(
                file_path,
                "session header is missing timestamp",
            ));
        }
    }
    match object.get("cwd") {
        Some(Value::String(cwd)) if !cwd.is_empty() => {}
        _ => return Err(invalid_session(file_path, "session header is missing cwd")),
    }
    // `parentSession !== undefined && typeof !== "string"` — absent only; a JSON
    // `null` fails the string check like upstream.
    if let Some(parent_session) = object.get("parentSession")
        && !parent_session.is_string()
    {
        return Err(invalid_session(
            file_path,
            "session header parentSession must be a string",
        ));
    }
    // `metadata !== undefined && (typeof !== "object" || === null || isArray)`.
    if let Some(metadata) = object.get("metadata")
        && !metadata.is_object()
    {
        return Err(invalid_session(
            file_path,
            "session header metadata must be an object",
        ));
    }
    // All fields are validated above; the typed parse cannot fail here.
    serde_json::from_value(value)
        .map_err(|_| invalid_session(file_path, "first line is not a valid session header"))
}

/// `parseEntryLine` (jsonl-storage.ts:103-132) — per-field validation with upstream
/// messages, then typed deserialization (stricter than the upstream cast; see header).
fn parse_entry_line(
    line: &str,
    file_path: &str,
    line_number: usize,
) -> Result<SessionEntry, SessionError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|_| invalid_entry(file_path, line_number, "is not valid JSON"))?;
    let Some(object) = value.as_object() else {
        return Err(invalid_entry(
            file_path,
            line_number,
            "is not a valid session entry",
        ));
    };
    if !object.get("type").is_some_and(Value::is_string) {
        return Err(invalid_entry(
            file_path,
            line_number,
            "is missing entry type",
        ));
    }
    match object.get("id") {
        Some(Value::String(id)) if !id.is_empty() => {}
        _ => return Err(invalid_entry(file_path, line_number, "is missing entry id")),
    }
    // `parentId !== null && typeof !== "string"` — the key must be present (absent is
    // `undefined` upstream and fails the check) and `null` or a string.
    match object.get("parentId") {
        Some(Value::Null) | Some(Value::String(_)) => {}
        _ => {
            return Err(invalid_entry(
                file_path,
                line_number,
                "has invalid parentId",
            ));
        }
    }
    match object.get("timestamp") {
        Some(Value::String(timestamp)) if !timestamp.is_empty() => {}
        _ => {
            return Err(invalid_entry(
                file_path,
                line_number,
                "is missing timestamp",
            ));
        }
    }
    if object.get("type").and_then(Value::as_str) == Some("leaf") {
        // `targetId !== null && typeof !== "string"` — required on leaves
        // (jsonl-storage.ts:128-130).
        match object.get("targetId") {
            Some(Value::Null) | Some(Value::String(_)) => {}
            _ => {
                return Err(invalid_entry(
                    file_path,
                    line_number,
                    "has invalid targetId",
                ));
            }
        }
    }
    serde_json::from_value(value)
        .map_err(|_| invalid_entry(file_path, line_number, "is not a valid session entry"))
}

/// `headerToSessionMetadata` (jsonl-storage.ts:138-147).
fn header_to_session_metadata(header: &SessionHeaderLine, path: &str) -> JsonlSessionMetadata {
    JsonlSessionMetadata {
        base: SessionMetadata {
            id: header.id.clone(),
            created_at: header.timestamp.clone(),
        },
        cwd: header.cwd.clone(),
        path: path.to_owned(),
        parent_session_path: header.parent_session.clone(),
        metadata: header.metadata.clone(),
    }
}

/// `loadJsonlSessionMetadata` (jsonl-storage.ts:149-160) — reads only the first line
/// (`readTextLines` with `maxLines: 1`).
pub async fn load_jsonl_session_metadata(
    fs: &dyn FileSystem,
    file_path: &str,
) -> Result<JsonlSessionMetadata, SessionError> {
    let lines = get_file_system_result_or_throw(
        fs.read_text_lines(
            file_path,
            ReadTextLinesOptions {
                max_lines: Some(1),
                abort_signal: None,
            },
        )
        .await,
        format!("Failed to read session header {file_path}"),
    )?;
    if let Some(line) = lines.first()
        && !line.trim().is_empty()
    {
        return Ok(header_to_session_metadata(
            &parse_header_line(line, file_path)?,
            file_path,
        ));
    }
    Err(invalid_session(file_path, "missing session header"))
}

/// `loadJsonlStorage` (jsonl-storage.ts:162-185) — full load: header, entries, and the
/// leaf replayed from the last entry line. Blank lines are dropped before parsing, and
/// entry error line numbers refer to the filtered lines like upstream.
async fn load_jsonl_storage(
    fs: &dyn FileSystem,
    file_path: &str,
) -> Result<(SessionHeaderLine, Vec<SessionEntry>, Option<String>), SessionError> {
    let content = get_file_system_result_or_throw(
        fs.read_text_file(file_path, None).await,
        format!("Failed to read session {file_path}"),
    )?;
    let lines: Vec<&str> = content
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Err(invalid_session(file_path, "missing session header"));
    }
    let header = parse_header_line(lines[0], file_path)?;
    let mut entries = Vec::new();
    let mut leaf_id: Option<String> = None;
    for (index, line) in lines.iter().enumerate().skip(1) {
        let entry = parse_entry_line(line, file_path, index + 1)?;
        leaf_id = leaf_id_after_entry(&entry);
        entries.push(entry);
    }
    Ok((header, entries, leaf_id))
}

/// Serialize one entry line (`JSON.stringify(entry)` + `\n`). Cannot fail for valid
/// data; the error is unreachable in practice.
fn entry_line(entry: &SessionEntry) -> Result<String, SessionError> {
    serde_json::to_string(entry).map_err(|error| {
        SessionError::new(
            SessionErrorCode::Storage,
            format!("Failed to serialize session entry {}: {error}", entry.id()),
        )
    })
}

/// Mutable storage state (interior mutability, see header note).
struct JsonlState {
    entries: Vec<SessionEntry>,
    by_id: HashMap<String, SessionEntry>,
    labels_by_id: HashMap<String, String>,
    current_leaf_id: Option<String>,
}

/// `JsonlSessionStorage` (jsonl-storage.ts:187-375).
pub struct JsonlSessionStorage {
    fs: Arc<dyn FileSystem>,
    file_path: String,
    metadata: JsonlSessionMetadata,
    state: Mutex<JsonlState>,
}

impl JsonlSessionStorage {
    /// Private constructor (jsonl-storage.ts:196-210).
    fn from_parts(
        fs: Arc<dyn FileSystem>,
        file_path: String,
        header: SessionHeaderLine,
        entries: Vec<SessionEntry>,
        leaf_id: Option<String>,
    ) -> Self {
        let mut by_id = HashMap::with_capacity(entries.len());
        for entry in &entries {
            by_id.insert(entry.id().to_owned(), entry.clone());
        }
        let labels_by_id = build_labels_by_id(&entries);
        let metadata = header_to_session_metadata(&header, &file_path);
        Self {
            fs,
            file_path,
            metadata,
            state: Mutex::new(JsonlState {
                entries,
                by_id,
                labels_by_id,
                current_leaf_id: leaf_id,
            }),
        }
    }

    /// `JsonlSessionStorage.open` (jsonl-storage.ts:212-215).
    pub async fn open(fs: Arc<dyn FileSystem>, file_path: &str) -> Result<Self, SessionError> {
        let (header, entries, leaf_id) = load_jsonl_storage(fs.as_ref(), file_path).await?;
        Ok(Self::from_parts(
            fs,
            file_path.to_owned(),
            header,
            entries,
            leaf_id,
        ))
    }

    /// `JsonlSessionStorage.create` (jsonl-storage.ts:217-241) — writes the header
    /// line (timestamp via `new Date().toISOString()`) and returns an empty storage.
    pub async fn create(
        fs: Arc<dyn FileSystem>,
        file_path: &str,
        options: JsonlSessionStorageCreateOptions,
    ) -> Result<Self, SessionError> {
        let header = SessionHeaderLine {
            r#type: "session".to_owned(),
            version: CURRENT_SESSION_VERSION,
            id: options.session_id,
            timestamp: now_iso8601(),
            cwd: options.cwd,
            parent_session: options.parent_session_path,
            metadata: options.metadata,
        };
        let line = serde_json::to_string(&header).map_err(|error| {
            SessionError::new(
                SessionErrorCode::Storage,
                format!("Failed to serialize session header {file_path}: {error}"),
            )
        })?;
        get_file_system_result_or_throw(
            fs.write_file(file_path, format!("{line}\n").as_bytes(), None)
                .await,
            format!("Failed to create session {file_path}"),
        )?;
        Ok(Self::from_parts(
            fs,
            file_path.to_owned(),
            header,
            Vec::new(),
            None,
        ))
    }
}

#[async_trait]
impl SessionStorage for JsonlSessionStorage {
    type Metadata = JsonlSessionMetadata;

    /// `getMetadata` (jsonl-storage.ts:243-245).
    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError> {
        Ok(self.metadata.clone())
    }

    /// `getLeafId` (jsonl-storage.ts:247-252) — validates the leaf against `byId`.
    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        let state = self.state.lock().await;
        if let Some(id) = &state.current_leaf_id
            && !state.by_id.contains_key(id)
        {
            return Err(SessionError::new(
                SessionErrorCode::InvalidSession,
                format!("Entry {id} not found"),
            ));
        }
        Ok(state.current_leaf_id.clone())
    }

    /// `setLeafId` (jsonl-storage.ts:254-272) — appends a `leaf` entry to the file
    /// first; memory is updated only after the append succeeds. The guard is held
    /// across the append so concurrent appends keep file and memory order aligned
    /// (see header note).
    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError> {
        let mut state = self.state.lock().await;
        if let Some(id) = &leaf_id
            && !state.by_id.contains_key(id)
        {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {id} not found"),
            ));
        }
        let entry = SessionEntry::Leaf(LeafEntry {
            id: generate_entry_id(&state.by_id),
            parent_id: state.current_leaf_id.clone(),
            timestamp: now_iso8601(),
            target_id: leaf_id.clone(),
        });
        let line = entry_line(&entry)?;
        get_file_system_result_or_throw(
            self.fs
                .append_file(&self.file_path, format!("{line}\n").as_bytes(), None)
                .await,
            format!("Failed to append session leaf {}", entry.id()),
        )?;
        state.entries.push(entry.clone());
        state.by_id.insert(entry.id().to_owned(), entry);
        state.current_leaf_id = leaf_id;
        Ok(())
    }

    /// `createEntryId` (jsonl-storage.ts:274-276).
    async fn create_entry_id(&self) -> Result<String, SessionError> {
        let state = self.state.lock().await;
        Ok(generate_entry_id(&state.by_id))
    }

    /// `appendEntry` (jsonl-storage.ts:278-287) — appends the line, then updates
    /// memory, the label cache, and the leaf. Guard held across the append (see
    /// header note).
    async fn append_entry(&self, entry: SessionEntry) -> Result<(), SessionError> {
        let mut state = self.state.lock().await;
        let line = entry_line(&entry)?;
        get_file_system_result_or_throw(
            self.fs
                .append_file(&self.file_path, format!("{line}\n").as_bytes(), None)
                .await,
            format!("Failed to append session entry {}", entry.id()),
        )?;
        state.entries.push(entry.clone());
        state.by_id.insert(entry.id().to_owned(), entry.clone());
        update_label_cache(&mut state.labels_by_id, &entry);
        state.current_leaf_id = leaf_id_after_entry(&entry);
        Ok(())
    }

    /// `getEntry` (jsonl-storage.ts:289-291).
    async fn get_entry(&self, id: &str) -> Result<Option<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        Ok(state.by_id.get(id).cloned())
    }

    /// `findEntries<TType>` (jsonl-storage.ts:293-297) — filter by the `entry.type`
    /// tag (types.rs note: per-type extraction has no Rust equivalent).
    async fn find_entries(&self, entry_type: &str) -> Result<Vec<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        Ok(state
            .entries
            .iter()
            .filter(|entry| entry.type_tag() == entry_type)
            .cloned()
            .collect())
    }

    /// `getLabel` (jsonl-storage.ts:299-301).
    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        let state = self.state.lock().await;
        Ok(state.labels_by_id.get(id).cloned())
    }

    /// `getSessionName` (jsonl-storage.ts:303-306) — the last `session_info` entry's
    /// trimmed name, or none. Scans `entries` directly (see header note).
    async fn get_session_name(&self) -> Result<Option<String>, SessionError> {
        let state = self.state.lock().await;
        let name = state.entries.iter().rev().find_map(|entry| match entry {
            SessionEntry::SessionInfo(info) => info.name.as_deref().map(str::trim),
            _ => None,
        });
        Ok(match name {
            Some(name) if !name.is_empty() => Some(name.to_owned()),
            _ => None,
        })
    }

    /// `getSessionStats` (jsonl-storage.ts:308-348) — usage from assistant messages
    /// and `compaction` / `branch_summary` entries. The upstream numeric guards
    /// (jsonl-storage.ts:326-335) are structurally unreachable for typed data (see
    /// header note).
    async fn get_session_stats(&self) -> Result<SessionStats, SessionError> {
        let state = self.state.lock().await;
        let mut stats = SessionStats::default();
        for entry in &state.entries {
            if matches!(entry, SessionEntry::Message(_)) {
                stats.message_count = stats.message_count.saturating_add(1);
            }
            let usage = match entry {
                SessionEntry::Message(MessageEntry {
                    message: AgentMessage::Assistant(assistant),
                    ..
                }) => Some(&assistant.usage),
                SessionEntry::Compaction(compaction) => compaction.usage.as_ref(),
                SessionEntry::BranchSummary(branch_summary) => branch_summary.usage.as_ref(),
                _ => None,
            };
            if let Some(usage) = usage {
                stats.cached_tokens = stats.cached_tokens.saturating_add(usage.cache_read);
                stats.uncached_tokens = stats
                    .uncached_tokens
                    .saturating_add(usage.input.saturating_add(usage.cache_write));
                stats.total_tokens = stats.total_tokens.saturating_add(
                    usage
                        .input
                        .saturating_add(usage.output)
                        .saturating_add(usage.cache_read)
                        .saturating_add(usage.cache_write),
                );
                stats.cost_total += usage.cost.total;
            }
        }
        Ok(stats)
    }

    /// `getPathToRootOrCompaction` (jsonl-storage.ts:350-369) — walk parents from the
    /// leaf, prepending to the path; stops at a compaction with `retainedTail`, at the
    /// compaction's `firstKeptEntryId`, or at the root.
    async fn get_path_to_root_or_compaction(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        let Some(leaf_id) = leaf_id else {
            return Ok(Vec::new());
        };
        let Some(mut current) = state.by_id.get(leaf_id).cloned() else {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {leaf_id} not found"),
            ));
        };
        let mut stop_at_entry_id: Option<String> = None;
        let mut path: Vec<SessionEntry> = Vec::new();
        loop {
            path.push(current.clone());
            if stop_at_entry_id.as_deref() == Some(current.id()) {
                break;
            }
            if let SessionEntry::Compaction(compaction) = &current {
                // Self-contained compaction (`retainedTail` present): path head.
                if compaction.retained_tail.is_some() {
                    break;
                }
                stop_at_entry_id = compaction.first_kept_entry_id.clone();
            }
            let Some(parent_id) = current.parent_id() else {
                break;
            };
            let Some(parent) = state.by_id.get(parent_id).cloned() else {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {parent_id} not found"),
                ));
            };
            current = parent;
        }
        path.reverse();
        Ok(path)
    }

    /// `getEntries` (jsonl-storage.ts:371-375) — JS `Array.prototype.slice` clamps
    /// both bounds, so out-of-range cursors yield `[]`.
    async fn get_entries(
        &self,
        options: SessionEntryCursorOptions,
    ) -> Result<Vec<SessionEntry>, SessionError> {
        let state = self.state.lock().await;
        let len = state.entries.len();
        let start = options.after_entry_seq.unwrap_or(0).min(len);
        let end = match options.limit {
            Some(limit) => start.saturating_add(limit).min(len),
            None => len,
        };
        Ok(state.entries[start..end].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// `expect_err` without a `T: Debug` bound (the storage types are not `Debug`).
    fn expect_err<T, E>(result: Result<T, E>, message: &str) -> E {
        match result {
            Ok(_) => panic!("{message}: expected an error"),
            Err(error) => error,
        }
    }
    use crate::harness::session::repo_utils::test_support::{
        TestFs, assistant_message, assistant_message_with_usage, message_entry, usage, user_message,
    };
    use crate::harness::types::SessionEntryCursorOptions;
    use crate::session::{BranchSummaryEntry, CompactionEntry, LabelEntry, LeafEntry};

    use super::*;

    fn create_options(cwd: &str, session_id: &str) -> JsonlSessionStorageCreateOptions {
        JsonlSessionStorageCreateOptions {
            cwd: cwd.to_owned(),
            session_id: session_id.to_owned(),
            parent_session_path: None,
            metadata: None,
        }
    }

    fn path(fs: &TestFs, name: &str) -> String {
        fs.root().join(name).to_string_lossy().into_owned()
    }

    fn header_line(id: &str, timestamp: &str, cwd: &str) -> Value {
        json!({
            "type": "session",
            "version": 3,
            "id": id,
            "timestamp": timestamp,
            "cwd": cwd,
        })
    }

    fn write_raw(file_path: &str, content: &str) {
        std::fs::write(file_path, content).expect("write session file");
    }

    async fn entry_ids(storage: &JsonlSessionStorage) -> Vec<String> {
        storage
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries")
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn test_open_missing_file_throws_not_found() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let error = expect_err(JsonlSessionStorage::open(fs, &file_path).await, "open");
        assert_eq!(error.code, SessionErrorCode::NotFound);
    }

    #[tokio::test]
    async fn test_create_writes_header_line() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            create_options("/repo", "session-1"),
        )
        .await
        .expect("create");
        assert!(std::path::Path::new(&file_path).exists());
        let raw = std::fs::read_to_string(&file_path).expect("read file");
        let lines: Vec<&str> = raw.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1);
        let header: Value = serde_json::from_str(lines[0]).expect("header json");
        assert_eq!(header["type"], "session");
        assert_eq!(header["version"], 3);
        assert_eq!(header["id"], "session-1");
        assert_eq!(header["cwd"], "/repo");
        assert_eq!(storage.get_leaf_id().await.expect("leaf"), None);
        assert!(
            storage
                .get_entries(SessionEntryCursorOptions::default())
                .await
                .expect("entries")
                .is_empty()
        );
        storage
            .append_entry(message_entry(
                "user-1",
                None,
                "2026-01-01T00:00:00.000Z",
                user_message("one"),
            ))
            .await
            .expect("append");
        let raw = std::fs::read_to_string(&file_path).expect("read file");
        let lines: Vec<&str> = raw.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).expect("line 0");
        let second: Value = serde_json::from_str(lines[1]).expect("line 1");
        assert_eq!(first["type"], "session");
        assert_eq!(second["id"], "user-1");
    }

    #[tokio::test]
    async fn test_open_rejects_malformed_session_header() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        write_raw(&file_path, "not json\n");
        let error = expect_err(JsonlSessionStorage::open(fs, &file_path).await, "open");
        assert_eq!(error.code, SessionErrorCode::InvalidSession);
        assert!(
            error
                .message
                .contains("first line is not a valid session header")
        );
    }

    #[tokio::test]
    async fn test_open_rejects_unsupported_session_version() {
        // `version !== 3` — no v1/v2 migration (jsonl-storage.ts:77).
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        write_raw(
            &file_path,
            &format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "type": "session",
                    "version": 2,
                    "id": "session-1",
                    "timestamp": "2026-01-01T00:00:00.000Z",
                    "cwd": "/repo",
                }))
                .expect("json")
            ),
        );
        let error = expect_err(JsonlSessionStorage::open(fs, &file_path).await, "open");
        assert_eq!(error.code, SessionErrorCode::InvalidSession);
        assert!(error.message.contains("unsupported session version"));
    }

    #[tokio::test]
    async fn test_open_rejects_header_with_missing_or_invalid_fields() {
        let fs = TestFs::new();
        let cases: Vec<(&str, Value)> = vec![
            (
                "first line is not a valid session header",
                json!({"version": 3, "id": "s", "timestamp": "t", "cwd": "/"}),
            ),
            (
                "session header is missing id",
                json!({"type": "session", "version": 3, "timestamp": "t", "cwd": "/"}),
            ),
            (
                "session header is missing timestamp",
                json!({"type": "session", "version": 3, "id": "s", "cwd": "/"}),
            ),
            (
                "session header is missing cwd",
                json!({"type": "session", "version": 3, "id": "s", "timestamp": "t"}),
            ),
            (
                "session header parentSession must be a string",
                json!({"type": "session", "version": 3, "id": "s", "timestamp": "t", "cwd": "/", "parentSession": 5}),
            ),
            (
                "session header metadata must be an object",
                json!({"type": "session", "version": 3, "id": "s", "timestamp": "t", "cwd": "/", "metadata": "profile"}),
            ),
            (
                "session header metadata must be an object",
                json!({"type": "session", "version": 3, "id": "s", "timestamp": "t", "cwd": "/", "metadata": null}),
            ),
            (
                "session header metadata must be an object",
                json!({"type": "session", "version": 3, "id": "s", "timestamp": "t", "cwd": "/", "metadata": [1]}),
            ),
        ];
        for (expected, header) in cases {
            let file_path = path(&fs, "session.jsonl");
            write_raw(
                &file_path,
                &format!("{}\n", serde_json::to_string(&header).expect("json")),
            );
            let error = expect_err(
                JsonlSessionStorage::open(fs.clone(), &file_path).await,
                "open",
            );
            assert_eq!(error.code, SessionErrorCode::InvalidSession);
            assert!(
                error.message.contains(expected),
                "for header {header}: {}",
                error.message
            );
        }
    }

    #[tokio::test]
    async fn test_open_rejects_malformed_entry_lines() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let entry = message_entry(
            "entry-1",
            None,
            "2026-01-01T00:00:00.000Z",
            user_message("one"),
        );
        let content = format!(
            "{}\nnot json\n{}\n",
            serde_json::to_string(&header_line(
                "session-1",
                "2026-01-01T00:00:00.000Z",
                "/repo"
            ))
            .expect("json"),
            serde_json::to_string(&entry).expect("json")
        );
        write_raw(&file_path, &content);
        let error = expect_err(JsonlSessionStorage::open(fs, &file_path).await, "open");
        assert_eq!(error.code, SessionErrorCode::InvalidEntry);
        assert!(error.message.contains("line 2"), "{}", error.message);
    }

    #[tokio::test]
    async fn test_open_rejects_entries_with_invalid_fields() {
        let fs = TestFs::new();
        let cases: Vec<(&str, Value)> = vec![
            (
                "is missing entry type",
                json!({"id": "e", "parentId": null, "timestamp": "t"}),
            ),
            (
                "is missing entry id",
                json!({"type": "message", "parentId": null, "timestamp": "t"}),
            ),
            (
                "has invalid parentId",
                json!({"type": "message", "id": "e", "timestamp": "t"}),
            ),
            (
                "has invalid parentId",
                json!({"type": "message", "id": "e", "parentId": 5, "timestamp": "t"}),
            ),
            (
                "is missing timestamp",
                json!({"type": "message", "id": "e", "parentId": null}),
            ),
            (
                "has invalid targetId",
                json!({"type": "leaf", "id": "l", "parentId": null, "timestamp": "t"}),
            ),
            (
                "has invalid targetId",
                json!({"type": "leaf", "id": "l", "parentId": null, "timestamp": "t", "targetId": 5}),
            ),
        ];
        for (expected, entry) in cases {
            let file_path = path(&fs, "session.jsonl");
            let content = format!(
                "{}\n{}\n",
                serde_json::to_string(&header_line(
                    "session-1",
                    "2026-01-01T00:00:00.000Z",
                    "/repo"
                ))
                .expect("json"),
                serde_json::to_string(&entry).expect("json")
            );
            write_raw(&file_path, &content);
            let error = expect_err(
                JsonlSessionStorage::open(fs.clone(), &file_path).await,
                "open",
            );
            assert_eq!(error.code, SessionErrorCode::InvalidEntry);
            assert!(
                error.message.contains(expected),
                "for entry {entry}: {}",
                error.message
            );
        }
    }

    #[tokio::test]
    async fn test_create_and_load_session_metadata_from_header() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            JsonlSessionStorageCreateOptions {
                cwd: "/repo".to_owned(),
                session_id: "session-1".to_owned(),
                parent_session_path: Some("/tmp/parent.jsonl".to_owned()),
                metadata: None,
            },
        )
        .await
        .expect("create");
        let metadata = storage.get_metadata().await.expect("metadata");
        assert_eq!(metadata.base.id, "session-1");
        assert_eq!(metadata.cwd, "/repo");
        assert_eq!(metadata.path, file_path);
        assert_eq!(
            metadata.parent_session_path.as_deref(),
            Some("/tmp/parent.jsonl")
        );
        storage
            .append_entry(message_entry(
                "user-1",
                None,
                "2026-01-01T00:00:00.000Z",
                user_message("one"),
            ))
            .await
            .expect("append");
        assert_eq!(
            load_jsonl_session_metadata(fs.as_ref(), &file_path)
                .await
                .expect("load"),
            metadata
        );
    }

    #[tokio::test]
    async fn test_header_metadata_roundtrips_through_open_and_load() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let mut header_metadata = Map::new();
        header_metadata.insert("profile".to_owned(), json!("reviewer"));
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            JsonlSessionStorageCreateOptions {
                cwd: "/repo".to_owned(),
                session_id: "session-1".to_owned(),
                parent_session_path: None,
                metadata: Some(header_metadata.clone()),
            },
        )
        .await
        .expect("create");
        assert_eq!(
            storage.get_metadata().await.expect("metadata").metadata,
            Some(header_metadata.clone())
        );
        let loaded = JsonlSessionStorage::open(fs.clone(), &file_path)
            .await
            .expect("open");
        assert_eq!(
            loaded.get_metadata().await.expect("metadata").metadata,
            Some(header_metadata.clone())
        );
        assert_eq!(
            load_jsonl_session_metadata(fs.as_ref(), &file_path)
                .await
                .expect("load")
                .metadata,
            Some(header_metadata)
        );
    }

    #[tokio::test]
    async fn test_header_metadata_omitted_when_not_provided() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        JsonlSessionStorage::create(fs.clone(), &file_path, create_options("/repo", "session-1"))
            .await
            .expect("create");
        let raw = std::fs::read_to_string(&file_path).expect("read file");
        let header: Value = serde_json::from_str(raw.trim()).expect("header json");
        assert!(header.get("metadata").is_none());
        assert_eq!(
            load_jsonl_session_metadata(fs.as_ref(), &file_path)
                .await
                .expect("load")
                .metadata,
            None
        );
    }

    #[tokio::test]
    async fn test_open_loads_entries_and_reconstructs_leaf() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            create_options("/repo", "session-1"),
        )
        .await
        .expect("create");
        storage
            .append_entry(message_entry(
                "root",
                None,
                "2026-01-01T00:00:00.000Z",
                user_message("root"),
            ))
            .await
            .expect("append");
        storage
            .append_entry(message_entry(
                "child",
                Some("root"),
                "2026-01-01T00:00:01.000Z",
                assistant_message("child"),
            ))
            .await
            .expect("append");
        let loaded = JsonlSessionStorage::open(fs.clone(), &file_path)
            .await
            .expect("open");
        assert_eq!(
            loaded.get_leaf_id().await.expect("leaf"),
            Some("child".to_owned())
        );
        assert_eq!(entry_ids(&loaded).await, ["root", "child"]);
        loaded
            .set_leaf_id(Some("root".to_owned()))
            .await
            .expect("set leaf");
        let reloaded = JsonlSessionStorage::open(fs.clone(), &file_path)
            .await
            .expect("open");
        assert_eq!(
            reloaded.get_leaf_id().await.expect("leaf"),
            Some("root".to_owned())
        );
        let last = reloaded
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries")
            .pop()
            .expect("last entry");
        assert!(
            matches!(last, SessionEntry::Leaf(LeafEntry { target_id: Some(target), .. }) if target == "root")
        );
        let path_ids = loaded
            .get_path_to_root_or_compaction(Some("child"))
            .await
            .expect("path")
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(path_ids, ["root", "child"]);
    }

    #[tokio::test]
    async fn test_set_leaf_id_rejects_missing_entries() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            create_options("/repo", "session-1"),
        )
        .await
        .expect("create");
        let error = expect_err(
            storage.set_leaf_id(Some("missing".to_owned())).await,
            "error",
        );
        assert_eq!(error.code, SessionErrorCode::NotFound);
        assert!(error.message.contains("Entry missing not found"));
    }

    #[tokio::test]
    async fn test_find_entries_filters_by_type() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            create_options("/repo", "session-1"),
        )
        .await
        .expect("create");
        storage
            .append_entry(message_entry(
                "entry-1",
                None,
                "2026-01-01T00:00:00.000Z",
                user_message("one"),
            ))
            .await
            .expect("append");
        let found = storage.find_entries("message").await.expect("find");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id(), "entry-1");
        assert!(
            storage
                .find_entries("session_info")
                .await
                .expect("find")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_maintains_label_lookup() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            create_options("/repo", "session-1"),
        )
        .await
        .expect("create");
        storage
            .append_entry(message_entry(
                "entry-1",
                None,
                "2026-01-01T00:00:00.000Z",
                user_message("one"),
            ))
            .await
            .expect("append");
        assert_eq!(storage.get_label("entry-1").await.expect("label"), None);
        storage
            .append_entry(SessionEntry::Label(LabelEntry {
                id: "label-1".to_owned(),
                parent_id: Some("entry-1".to_owned()),
                timestamp: "2026-01-01T00:00:01.000Z".to_owned(),
                target_id: "entry-1".to_owned(),
                label: Some("checkpoint".to_owned()),
            }))
            .await
            .expect("append");
        assert_eq!(
            storage.get_label("entry-1").await.expect("label"),
            Some("checkpoint".to_owned())
        );
        storage
            .append_entry(SessionEntry::Label(LabelEntry {
                id: "label-2".to_owned(),
                parent_id: Some("label-1".to_owned()),
                timestamp: "2026-01-01T00:00:02.000Z".to_owned(),
                target_id: "entry-1".to_owned(),
                label: None,
            }))
            .await
            .expect("append");
        assert_eq!(storage.get_label("entry-1").await.expect("label"), None);
        let loaded = JsonlSessionStorage::open(fs, &file_path)
            .await
            .expect("open");
        assert_eq!(loaded.get_label("entry-1").await.expect("label"), None);
    }

    #[tokio::test]
    async fn test_get_session_stats_includes_summary_entry_usage() {
        let fs = TestFs::new();
        let file_path = path(&fs, "session.jsonl");
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &file_path,
            create_options("/repo", "session-1"),
        )
        .await
        .expect("create");
        storage
            .append_entry(message_entry(
                "assistant",
                None,
                "2026-01-01T00:00:00.000Z",
                assistant_message_with_usage("reply", usage(10, 20, 30, 40, 100, 1.0)),
            ))
            .await
            .expect("append");
        storage
            .append_entry(SessionEntry::Compaction(CompactionEntry {
                id: "compaction".to_owned(),
                parent_id: Some("assistant".to_owned()),
                timestamp: "2026-01-01T00:00:01.000Z".to_owned(),
                summary: "summary".to_owned(),
                first_kept_entry_id: Some("assistant".to_owned()),
                tokens_before: 1234,
                retained_tail: None,
                details: None,
                usage: Some(usage(1, 2, 3, 4, 10, 0.1)),
                from_hook: None,
                system_message: None,
            }))
            .await
            .expect("append");
        storage
            .append_entry(SessionEntry::BranchSummary(BranchSummaryEntry {
                id: "branch-summary".to_owned(),
                parent_id: Some("compaction".to_owned()),
                timestamp: "2026-01-01T00:00:02.000Z".to_owned(),
                from_id: "assistant".to_owned(),
                summary: "branch".to_owned(),
                details: None,
                usage: Some(usage(5, 6, 7, 8, 26, 0.26)),
                from_hook: None,
            }))
            .await
            .expect("append");
        assert_eq!(
            storage.get_session_stats().await.expect("stats"),
            SessionStats {
                message_count: 1,
                cached_tokens: 40,
                uncached_tokens: 68,
                total_tokens: 136,
                cost_total: 1.36,
            }
        );
    }

    #[tokio::test]
    async fn test_load_metadata_reads_only_the_header_line() {
        let fs = TestFs::new_denying_read_text_file();
        let file_path = path(&fs, "session.jsonl");
        write_raw(
            &file_path,
            &format!(
                "{}\n",
                serde_json::to_string(&header_line(
                    "session-1",
                    "2026-01-01T00:00:00.000Z",
                    "/repo",
                ))
                .expect("json")
            ),
        );
        let metadata = load_jsonl_session_metadata(fs.as_ref(), &file_path)
            .await
            .expect("load");
        assert_eq!(metadata.base.id, "session-1");
        assert_eq!(metadata.base.created_at, "2026-01-01T00:00:00.000Z");
        assert_eq!(metadata.cwd, "/repo");
        assert_eq!(metadata.path, file_path);
        assert_eq!(metadata.parent_session_path, None);
        // `readTextLines(..., { maxLines: 1 })` (jsonl-storage.ts:154).
        assert_eq!(fs.last_max_lines(), Some(1));
    }
}
