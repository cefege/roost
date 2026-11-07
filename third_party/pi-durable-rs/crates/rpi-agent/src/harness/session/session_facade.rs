//! Port of `packages/agent/src/harness/session/session.ts` @ pi 0.82.1 (2efa728) —
//! the `Session` facade class and the context-building helpers. The module
//! `harness::session` maps to `session.ts` (T16 plan); the code lives here so the
//! module root stays a pure declaration file.
//!
//! Intentional differences:
//! - The upstream `Session` class becomes two items: the type-layer
//!   [`SessionTrait`] (types.rs:1019 — the full class surface as a trait) and the
//!   concrete [`Session`] struct here implementing it. `toSession`
//!   (repo-utils.ts:20-22) wraps storage backends with the struct.
//! - `ContextEntryTransform` / `CustomEntryContextMessageProjector` are
//!   `Arc<dyn Fn + Send + Sync>` (types.rs) instead of TS function types — `Arc`
//!   so merged option sets can share transforms/projectors without cloning the
//!   closure (session.ts:192-200).
//! - `appendCompaction`'s trailing parameters (`details` / `fromHook` / `usage` /
//!   `retainedTail`, session.ts:260-281) collapse into [`AppendCompactionOptions`];
//!   `moveTo`'s inline summary object (session.ts:338-340) into [`MoveToSummary`]
//!   (both types.rs).
//! - `sessionEntryToContextMessages` (session.ts:103-136) delegates the shared
//!   variants to the crate-root port (`crate::session::session_entry_to_context_messages`,
//!   session-manager.ts:383-408 — same behavior for message / custom_message /
//!   compaction / branch_summary) and only layers the harness-only `custom`
//!   projector dispatch on top (session.ts:132-134).
//! - The timestamp helper (`new Date().toISOString()`) is `repo_utils::now_iso8601`
//!   (shared with the storage ports).
//! - Concurrent appends on one [`Session`] instance are serialized by an internal
//!   `tokio::sync::Mutex`: upstream's single-threaded event loop orders the
//!   `createEntryId → getLeafId → appendEntry` sequence implicitly (session.ts:
//!   219-227), Rust needs an explicit lock (see `with_append_lock`). The lock
//!   scopes to the instance — callers sharing one storage / session file across
//!   facade instances or processes must still be single-writer, as upstream.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use rpi_ai::types::UserContent;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::harness::types::{
    AppendCompactionOptions, MoveToSummary, Session as SessionTrait, SessionContext,
    SessionContextBuildOptions, SessionEntryCursorOptions, SessionError, SessionErrorCode,
    SessionModelRef, SessionStats, SessionStorage,
};
use crate::messages::AgentMessage;
use crate::session::{
    ActiveToolsChangeEntry, BranchSummaryEntry, CompactionEntry, CustomEntry, CustomMessageEntry,
    LabelEntry, MessageEntry, ModelChangeEntry, SessionEntry, SessionInfoEntry,
    ThinkingLevelChangeEntry,
};

use super::repo_utils::now_iso8601;

// ---------------------------------------------------------------------------
// Context building (session.ts:39-148)
// ---------------------------------------------------------------------------

/// `deriveSessionContextState` (session.ts:39-57) — last-writer-wins state over
/// the raw path entries (assistant messages and `model_change` entries both set
/// the model; `thinking_level_change` / `active_tools_change` set their fields).
struct DerivedSessionContextState {
    thinking_level: String,
    model: Option<SessionModelRef>,
    active_tool_names: Option<Vec<String>>,
}

fn derive_session_context_state(path_entries: &[SessionEntry]) -> DerivedSessionContextState {
    let mut thinking_level = "off".to_owned();
    let mut model: Option<SessionModelRef> = None;
    let mut active_tool_names: Option<Vec<String>> = None;
    for entry in path_entries {
        match entry {
            SessionEntry::ThinkingLevelChange(change) => {
                thinking_level = change.thinking_level.clone();
            }
            SessionEntry::ModelChange(change) => {
                model = Some(SessionModelRef {
                    provider: change.provider.clone(),
                    model_id: change.model_id.clone(),
                });
            }
            SessionEntry::Message(message) => {
                if let AgentMessage::Assistant(assistant) = &message.message {
                    model = Some(SessionModelRef {
                        provider: assistant.provider.clone(),
                        model_id: assistant.model.clone(),
                    });
                }
            }
            SessionEntry::ActiveToolsChange(change) => {
                active_tool_names = Some(change.active_tool_names.clone());
            }
            _ => {}
        }
    }
    DerivedSessionContextState {
        thinking_level,
        model,
        active_tool_names,
    }
}

/// `defaultContextEntryTransform` (session.ts:59-90) — the last compaction on the
/// path takes effect:
/// - no compaction: the path unchanged;
/// - `retainedTail` present: the compaction + everything after it (self-contained
///   checkpoint);
/// - otherwise: the compaction + entries from `firstKeptEntryId` up to it +
///   everything after it.
///
/// The compaction index is located during the reverse scan (session.ts:71 uses
/// `findIndex` from the start, but entry ids are unique per storage, so both
/// point at the same entry).
pub fn default_context_entry_transform(path_entries: &[SessionEntry]) -> Vec<SessionEntry> {
    let Some((compaction_idx, compaction)) =
        path_entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| match entry {
                SessionEntry::Compaction(compaction) => Some((index, compaction)),
                _ => None,
            })
    else {
        return path_entries.to_vec();
    };
    let compaction = compaction.clone();

    let mut entries: Vec<SessionEntry> = vec![SessionEntry::Compaction(compaction.clone())];
    if compaction.retained_tail.is_some() {
        // session.ts:72-77 — tail after the compaction only.
        entries.extend(path_entries[compaction_idx + 1..].iter().cloned());
        return entries;
    }
    if let Some(first_kept_entry_id) = &compaction.first_kept_entry_id {
        // session.ts:78-85 — entries from `firstKeptEntryId` (inclusive) up to
        // the compaction.
        let mut found_first_kept = false;
        for entry in &path_entries[..compaction_idx] {
            if entry.id() == first_kept_entry_id {
                found_first_kept = true;
            }
            if found_first_kept {
                entries.push(entry.clone());
            }
        }
    }
    entries.extend(path_entries[compaction_idx + 1..].iter().cloned());
    entries
}

/// `buildContextEntries` (session.ts:92-101) — default compaction transform
/// first, then the caller's transforms in order.
pub fn build_context_entries(
    path_entries: &[SessionEntry],
    options: &SessionContextBuildOptions,
) -> Vec<SessionEntry> {
    let mut entries = default_context_entry_transform(path_entries);
    for transform in &options.entry_transforms {
        entries = transform(&entries);
    }
    entries
}

/// `sessionEntryToContextMessages` (session.ts:103-136) — harness variant: the
/// shared variants (message / custom_message / compaction / branch_summary)
/// delegate to the crate-root port (see header note); only `custom` entries
/// differ — they are omitted unless a projector is configured for their
/// `customType` (session.ts:132-134).
pub fn session_entry_to_context_messages(
    entry: &SessionEntry,
    index: usize,
    entries: &[SessionEntry],
    options: &SessionContextBuildOptions,
) -> Vec<AgentMessage> {
    if let SessionEntry::Custom(custom) = entry {
        return match options.entry_projectors.get(&custom.custom_type) {
            Some(projector) => projector(custom, index, entries),
            None => Vec::new(),
        };
    }
    crate::session::session_entry_to_context_messages(entry)
}

/// `buildSessionContext` (session.ts:138-148) — derived state from the raw path
/// entries plus the projected messages from the transformed entries.
pub fn build_session_context(
    path_entries: &[SessionEntry],
    options: &SessionContextBuildOptions,
) -> SessionContext {
    let state = derive_session_context_state(path_entries);
    let context_entries = build_context_entries(path_entries, options);
    let messages = context_entries
        .iter()
        .enumerate()
        .flat_map(|(index, entry)| {
            session_entry_to_context_messages(entry, index, &context_entries, options)
        })
        .collect();
    SessionContext {
        messages,
        thinking_level: state.thinking_level,
        model: state.model,
        active_tool_names: state.active_tool_names,
    }
}

// ---------------------------------------------------------------------------
// Session facade (session.ts:150-359)
// ---------------------------------------------------------------------------

/// `Session` (session.ts:150-358) — the concrete facade over a
/// [`SessionStorage`], implementing the type-layer [`SessionTrait`]
/// (types.rs:1019). `toSession` (repo-utils.ts:20-22) wraps storage backends
/// with this struct.
///
/// All mutating operations (`append*` and `moveTo`) run under
/// [`Session::with_append_lock`], which serializes them per instance so the
/// `createEntryId → getLeafId → appendEntry` sequence cannot interleave (see
/// the module header). Cross-instance sharing of one storage still requires
/// the caller to be single-writer, matching upstream's single-threaded
/// semantics.
pub struct Session<TMetadata> {
    storage: Arc<dyn SessionStorage<Metadata = TMetadata>>,
    context_build_options: SessionContextBuildOptions,
    /// Serializes the mutating operations on this instance (see module header).
    append_lock: Mutex<()>,
}

impl<TMetadata> Session<TMetadata> {
    /// Constructor (session.ts:154-157).
    pub fn new(
        storage: Arc<dyn SessionStorage<Metadata = TMetadata>>,
        context_build_options: SessionContextBuildOptions,
    ) -> Self {
        Self {
            storage,
            context_build_options,
            append_lock: Mutex::new(()),
        }
    }

    /// `appendTypedEntry` (session.ts:214-217).
    async fn append_typed_entry(&self, entry: SessionEntry) -> Result<String, SessionError> {
        let id = entry.id().to_owned();
        self.storage.append_entry(entry).await?;
        Ok(id)
    }

    /// Serialize one mutating operation on this facade instance: the append
    /// sequence (`createEntryId` → `getLeafId` → `appendEntry`) must not
    /// interleave across concurrent appends, or a later entry can parent on a
    /// stale leaf. `moveTo` takes the lock too so its `setLeafId` cannot land
    /// between another append's `getLeafId` and `appendEntry`.
    ///
    /// Scope: the lock serializes within one `Session` instance only. Callers
    /// sharing a storage (or session file) across facade instances or
    /// processes must still be single-writer, matching upstream's
    /// single-threaded event loop (session.ts:219-227).
    async fn with_append_lock<T, F>(&self, operation: F) -> Result<T, SessionError>
    where
        F: Future<Output = Result<T, SessionError>>,
    {
        let _guard = self.append_lock.lock().await;
        operation.await
    }

    /// `mergeContextBuildOptions` (session.ts:192-200) — instance transforms
    /// first, then call transforms; call projectors override instance projectors
    /// per `customType`.
    fn merge_context_build_options(
        &self,
        options: &SessionContextBuildOptions,
    ) -> SessionContextBuildOptions {
        let mut entry_transforms = Vec::with_capacity(
            self.context_build_options.entry_transforms.len() + options.entry_transforms.len(),
        );
        entry_transforms.extend(self.context_build_options.entry_transforms.iter().cloned());
        entry_transforms.extend(options.entry_transforms.iter().cloned());

        let mut entry_projectors = HashMap::new();
        for (custom_type, projector) in &self.context_build_options.entry_projectors {
            entry_projectors.insert(custom_type.clone(), Arc::clone(projector));
        }
        for (custom_type, projector) in &options.entry_projectors {
            entry_projectors.insert(custom_type.clone(), Arc::clone(projector));
        }
        SessionContextBuildOptions {
            entry_transforms,
            entry_projectors,
        }
    }
}

#[async_trait]
impl<TMetadata: Send + Sync + 'static> SessionTrait for Session<TMetadata> {
    type Metadata = TMetadata;

    /// `getStorage` (session.ts:163-165).
    fn storage(&self) -> Arc<dyn SessionStorage<Metadata = Self::Metadata>> {
        Arc::clone(&self.storage)
    }

    /// `getMetadata` (session.ts:159-161).
    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError> {
        self.storage.get_metadata().await
    }

    /// `getLeafId` (session.ts:167-169).
    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.storage.get_leaf_id().await
    }

    /// `getEntry` (session.ts:171-173).
    async fn get_entry(&self, id: &str) -> Result<Option<SessionEntry>, SessionError> {
        self.storage.get_entry(id).await
    }

    /// `getEntries` (session.ts:175-177).
    async fn get_entries(
        &self,
        options: SessionEntryCursorOptions,
    ) -> Result<Vec<SessionEntry>, SessionError> {
        self.storage.get_entries(options).await
    }

    /// `getBranch` (session.ts:179-182) — path from `from_id` (or the current
    /// leaf) to the root or the latest compaction.
    async fn get_branch(&self, from_id: Option<&str>) -> Result<Vec<SessionEntry>, SessionError> {
        let leaf_id = match from_id {
            Some(id) => Some(id.to_owned()),
            None => self.storage.get_leaf_id().await?,
        };
        self.storage
            .get_path_to_root_or_compaction(leaf_id.as_deref())
            .await
    }

    /// `getLabel` (session.ts:202-204).
    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        self.storage.get_label(id).await
    }

    /// `getSessionStats` (session.ts:206-208).
    async fn get_session_stats(&self) -> Result<SessionStats, SessionError> {
        self.storage.get_session_stats().await
    }

    /// `getSessionName` (session.ts:210-212).
    async fn get_session_name(&self) -> Result<Option<String>, SessionError> {
        self.storage.get_session_name().await
    }

    /// `appendMessage` (session.ts:219-227).
    async fn append_message(&self, message: AgentMessage) -> Result<String, SessionError> {
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::Message(MessageEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                message,
            }))
            .await
        })
        .await
    }

    /// `appendThinkingLevelChange` (session.ts:229-237).
    async fn append_thinking_level_change(
        &self,
        thinking_level: &str,
    ) -> Result<String, SessionError> {
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::ThinkingLevelChange(
                ThinkingLevelChangeEntry {
                    id: self.storage.create_entry_id().await?,
                    parent_id: self.storage.get_leaf_id().await?,
                    timestamp: now_iso8601(),
                    thinking_level: thinking_level.to_owned(),
                },
            ))
            .await
        })
        .await
    }

    /// `appendModelChange` (session.ts:239-248).
    async fn append_model_change(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, SessionError> {
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::ModelChange(ModelChangeEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                provider: provider.to_owned(),
                model_id: model_id.to_owned(),
            }))
            .await
        })
        .await
    }

    /// `appendActiveToolsChange` (session.ts:250-258).
    async fn append_active_tools_change(
        &self,
        active_tool_names: &[String],
    ) -> Result<String, SessionError> {
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::ActiveToolsChange(ActiveToolsChangeEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                active_tool_names: active_tool_names.to_vec(),
            }))
            .await
        })
        .await
    }

    /// `appendCompaction` (session.ts:260-282).
    async fn append_compaction(
        &self,
        summary: &str,
        first_kept_entry_id: Option<&str>,
        tokens_before: u64,
        options: AppendCompactionOptions,
    ) -> Result<String, SessionError> {
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::Compaction(CompactionEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                summary: summary.to_owned(),
                first_kept_entry_id: first_kept_entry_id.map(str::to_owned),
                tokens_before,
                retained_tail: options.retained_tail,
                details: options.details,
                usage: options.usage,
                from_hook: options.from_hook,
                // #9548: the harness compaction form carries no checkpoint
                // (upstream harness session types keep the four-field entry).
                system_message: None,
            }))
            .await
        })
        .await
    }

    /// `appendCustomEntry` (session.ts:284-293).
    async fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::Custom(CustomEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                custom_type: custom_type.to_owned(),
                data,
            }))
            .await
        })
        .await
    }

    /// `appendCustomMessageEntry` (session.ts:295-311).
    async fn append_custom_message_entry(
        &self,
        custom_type: &str,
        content: UserContent,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, SessionError> {
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::CustomMessage(CustomMessageEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                custom_type: custom_type.to_owned(),
                content,
                display,
                details,
            }))
            .await
        })
        .await
    }

    /// `appendLabel` (session.ts:313-325) — missing targets are rejected with
    /// `not_found`.
    async fn append_label(
        &self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, SessionError> {
        if self.storage.get_entry(target_id).await?.is_none() {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {target_id} not found"),
            ));
        }
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::Label(LabelEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                target_id: target_id.to_owned(),
                label: label.map(str::to_owned),
            }))
            .await
        })
        .await
    }

    /// `appendSessionName` (session.ts:327-336) — `[\r\n]+` runs collapse to a
    /// single space (session.ts:328), then the name is trimmed.
    async fn append_session_name(&self, name: &str) -> Result<String, SessionError> {
        let mut sanitized = String::with_capacity(name.len());
        let mut in_newline_run = false;
        for ch in name.chars() {
            if ch == '\r' || ch == '\n' {
                if !in_newline_run {
                    sanitized.push(' ');
                    in_newline_run = true;
                }
            } else {
                in_newline_run = false;
                sanitized.push(ch);
            }
        }
        self.with_append_lock(async move {
            self.append_typed_entry(SessionEntry::SessionInfo(SessionInfoEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: self.storage.get_leaf_id().await?,
                timestamp: now_iso8601(),
                name: Some(sanitized.trim().to_owned()),
            }))
            .await
        })
        .await
    }

    /// `moveTo` (session.ts:338-358) — validates the target, moves the leaf
    /// (`None` = root), then optionally appends a `branch_summary` entry under
    /// the new leaf (returning its id).
    async fn move_to(
        &self,
        entry_id: Option<&str>,
        summary: Option<MoveToSummary>,
    ) -> Result<Option<String>, SessionError> {
        self.with_append_lock(async move {
            if let Some(entry_id) = entry_id
                && self.storage.get_entry(entry_id).await?.is_none()
            {
                return Err(SessionError::new(
                    SessionErrorCode::NotFound,
                    format!("Entry {entry_id} not found"),
                ));
            }
            self.storage
                .set_leaf_id(entry_id.map(str::to_owned))
                .await?;
            let Some(summary) = summary else {
                return Ok(None);
            };
            let entry_id_owned = entry_id.map(str::to_owned);
            self.append_typed_entry(SessionEntry::BranchSummary(BranchSummaryEntry {
                id: self.storage.create_entry_id().await?,
                parent_id: entry_id_owned.clone(),
                timestamp: now_iso8601(),
                from_id: entry_id_owned.unwrap_or_else(|| "root".to_owned()),
                summary: summary.summary,
                details: summary.details,
                usage: summary.usage,
                from_hook: summary.from_hook,
            }))
            .await
            .map(Some)
        })
        .await
    }

    /// `buildContextEntries` (session.ts:184-186).
    async fn build_context_entries(
        &self,
        options: SessionContextBuildOptions,
    ) -> Result<Vec<SessionEntry>, SessionError> {
        let branch = self.get_branch(None).await?;
        let merged = self.merge_context_build_options(&options);
        Ok(build_context_entries(&branch, &merged))
    }

    /// `buildContext` (session.ts:188-190).
    async fn build_context(
        &self,
        options: SessionContextBuildOptions,
    ) -> Result<SessionContext, SessionError> {
        let branch = self.get_branch(None).await?;
        let merged = self.merge_context_build_options(&options);
        Ok(build_session_context(&branch, &merged))
    }
}

#[cfg(test)]
mod tests {
    //! Port of `packages/agent/test/harness/session.test.ts` @ pi 0.82.1
    //! (2efa728) — the `runSessionSuite` parameterization over in-memory and
    //! JSONL storage is a shared generic body per test plus one wrapper per
    //! storage (see `session_suite!`). One extra test beyond upstream covers
    //! `active_tool_names` / model-from-assistant derivation
    //! (`deriveSessionContextState`, session.ts:39-57), which upstream's
    //! session.test.ts does not exercise.

    use std::collections::HashMap;
    use std::sync::Arc;

    use rpi_ai::types::{Usage, UsageCost, UserContent};
    use serde_json::{Value, json};

    use crate::harness::session::jsonl_storage::{
        JsonlSessionStorage, JsonlSessionStorageCreateOptions,
    };
    use crate::harness::session::memory_storage::{
        InMemorySessionStorage, InMemorySessionStorageOptions,
    };
    use crate::harness::session::repo_utils::test_support::{
        TestFs, assistant_message, user_message,
    };
    use crate::harness::types::{
        AppendCompactionOptions, ContextEntryTransform, CustomEntryContextMessageProjector,
        JsonlSessionMetadata, MoveToSummary, Session as SessionTrait, SessionContext,
        SessionContextBuildOptions, SessionEntryCursorOptions, SessionErrorCode, SessionMetadata,
        SessionModelRef, SessionStorage,
    };
    use crate::messages::AgentMessage;
    use crate::session::{CustomEntry, SessionEntry};

    use super::*;

    /// `runSessionSuite` storage wiring — in-memory backend
    /// (session.test.ts:231).
    fn memory_storage() -> Arc<dyn SessionStorage<Metadata = SessionMetadata>> {
        Arc::new(
            InMemorySessionStorage::new(InMemorySessionStorageOptions::default()).expect("storage"),
        )
    }

    /// `runSessionSuite` storage wiring — JSONL backend
    /// (session.test.ts:233-239): `JsonlSessionStorage.create` under a fresh
    /// temp dir, session file `session.jsonl`.
    async fn jsonl_storage(
        fs: Arc<TestFs>,
    ) -> Arc<dyn SessionStorage<Metadata = JsonlSessionMetadata>> {
        let path = fs
            .root()
            .join("session.jsonl")
            .to_string_lossy()
            .into_owned();
        let storage = JsonlSessionStorage::create(
            fs.clone(),
            &path,
            JsonlSessionStorageCreateOptions {
                cwd: fs.root().to_string_lossy().into_owned(),
                session_id: "session-1".to_owned(),
                parent_session_path: None,
                metadata: None,
            },
        )
        .await
        .expect("create");
        Arc::new(storage)
    }

    /// `new Session(storage, options)` (session.test.ts:26) — wraps the storage
    /// in the facade struct and upcasts to the trait object.
    fn build_session<M: Send + Sync + 'static>(
        storage: Arc<dyn SessionStorage<Metadata = M>>,
        options: SessionContextBuildOptions,
    ) -> Arc<dyn SessionTrait<Metadata = M>> {
        Arc::new(Session::new(storage, options))
    }

    fn memory_session() -> Arc<dyn SessionTrait<Metadata = SessionMetadata>> {
        build_session(memory_storage(), SessionContextBuildOptions::default())
    }

    async fn jsonl_session(
        fs: Arc<TestFs>,
    ) -> Arc<dyn SessionTrait<Metadata = JsonlSessionMetadata>> {
        build_session(
            jsonl_storage(fs).await,
            SessionContextBuildOptions::default(),
        )
    }

    /// `message.role` literal (upstream `message.role` comparisons).
    fn role_tag(message: &AgentMessage) -> &'static str {
        match message {
            AgentMessage::User(_) => "user",
            AgentMessage::Assistant(_) => "assistant",
            AgentMessage::ToolResult(_) => "toolResult",
            AgentMessage::BashExecution(_) => "bashExecution",
            AgentMessage::Custom(_) => "custom",
            AgentMessage::BranchSummary(_) => "branchSummary",
            AgentMessage::CompactionSummary(_) => "compactionSummary",
            AgentMessage::System(_) => "system",
        }
    }

    fn roles(context: &SessionContext) -> Vec<&'static str> {
        context.messages.iter().map(role_tag).collect()
    }

    /// `getTextData` (session.test.ts:11-17).
    fn get_text_data(data: Option<&Value>) -> String {
        match data {
            Some(Value::Object(map)) => map
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            _ => String::new(),
        }
    }

    /// The JSONL suite's `inspect` callback (session.test.ts:240-254): the
    /// session file holds the header line plus typed entries with string ids,
    /// including a `leaf` record.
    fn inspect_jsonl_file(fs: &TestFs) {
        let path = fs.root().join("session.jsonl");
        let raw = std::fs::read_to_string(&path).expect("read session file");
        let lines: Vec<&str> = raw.trim().split('\n').collect();
        assert!(lines.len() > 1, "expected more than one line");
        let header: Value = serde_json::from_str(lines[0]).expect("header json");
        assert_eq!(header["type"], "session");
        assert_eq!(header["version"], 3);
        let mut saw_leaf = false;
        for line in &lines[1..] {
            let entry: Value = serde_json::from_str(line).expect("entry json");
            assert_ne!(entry["type"], "entry");
            assert!(entry["id"].is_string());
            if entry["type"] == "leaf" {
                saw_leaf = true;
            }
        }
        assert!(saw_leaf, "expected a leaf entry");
    }

    /// `runSessionSuite` (session.test.ts:19-23) — each shared body runs once
    /// against the in-memory storage and once against the JSONL storage.
    /// (`$in_memory` / `$jsonl` / `$body` are separate idents: macro_rules
    /// cannot concatenate `in_memory_` + `$body` into one identifier.)
    macro_rules! session_suite {
        ($in_memory:ident, $jsonl:ident, $body:ident) => {
            #[tokio::test]
            async fn $in_memory() {
                $body(memory_session()).await;
            }

            #[tokio::test]
            async fn $jsonl() {
                let fs = TestFs::new();
                $body(jsonl_session(fs.clone()).await).await;
            }
        };
    }

    /// "appends messages and builds context in order" (session.test.ts:25-31).
    async fn appends_messages_and_builds_context_in_order<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_message(assistant_message("two"))
            .await
            .expect("append");
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(roles(&context), ["user", "assistant"]);
    }

    /// "tracks model and thinking level changes" (session.test.ts:33-41).
    async fn tracks_model_and_thinking_level_changes<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_model_change("openai", "gpt-4.1")
            .await
            .expect("append");
        session
            .append_thinking_level_change("high")
            .await
            .expect("append");
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(context.thinking_level, "high");
        assert_eq!(
            context.model,
            Some(SessionModelRef {
                provider: "openai".to_owned(),
                model_id: "gpt-4.1".to_owned(),
            })
        );
    }

    /// "supports branching by moving the leaf and appending a new branch"
    /// (session.test.ts:43-55).
    async fn supports_branching_by_moving_the_leaf_and_appending_a_new_branch<
        M: Send + Sync + 'static,
    >(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        let user1 = session
            .append_message(user_message("one"))
            .await
            .expect("append");
        let assistant1 = session
            .append_message(assistant_message("two"))
            .await
            .expect("append");
        session
            .append_message(user_message("three"))
            .await
            .expect("append");
        session.move_to(Some(&user1), None).await.expect("move");
        session
            .append_message(assistant_message("branched"))
            .await
            .expect("append");
        let branch = session.get_branch(None).await.expect("branch");
        let branch_ids: Vec<&str> = branch.iter().map(SessionEntry::id).collect();
        assert!(branch_ids.contains(&user1.as_str()));
        assert!(!branch_ids.contains(&assistant1.as_str()));
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(roles(&context), ["user", "assistant"]);
    }

    /// "supports moving the leaf to root" (session.test.ts:57-63).
    async fn supports_moving_the_leaf_to_root<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        assert_eq!(session.move_to(None, None).await.expect("move"), None);
        assert_eq!(session.get_leaf_id().await.expect("leaf"), None);
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert!(context.messages.is_empty());
    }

    /// "reconstructs compaction summaries in context" (session.test.ts:65-85).
    async fn reconstructs_compaction_summaries_in_context<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_message(assistant_message("two"))
            .await
            .expect("append");
        let user2 = session
            .append_message(user_message("three"))
            .await
            .expect("append");
        session
            .append_message(assistant_message("four"))
            .await
            .expect("append");
        session
            .append_compaction(
                "summary",
                Some(&user2),
                1234,
                AppendCompactionOptions {
                    retained_tail: Some(vec![user_message("three"), assistant_message("four")]),
                    ..Default::default()
                },
            )
            .await
            .expect("append");
        session
            .append_message(user_message("five"))
            .await
            .expect("append");
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(role_tag(&context.messages[0]), "compactionSummary");
        assert_eq!(context.messages.len(), 4);
        assert_eq!(
            roles(&context),
            ["compactionSummary", "user", "assistant", "user"]
        );
    }

    /// "supports moving with branch summary entries in context"
    /// (session.test.ts:87-96).
    async fn supports_moving_with_branch_summary_entries_in_context<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        let user1 = session
            .append_message(user_message("one"))
            .await
            .expect("append");
        let summary_id = session
            .move_to(
                Some(&user1),
                Some(MoveToSummary {
                    summary: "summary text".to_owned(),
                    ..Default::default()
                }),
            )
            .await
            .expect("move")
            .expect("summary id");
        let summary_entry = session
            .get_entry(&summary_id)
            .await
            .expect("entry")
            .expect("found");
        let SessionEntry::BranchSummary(branch) = summary_entry else {
            panic!("expected branch_summary entry");
        };
        assert_eq!(branch.parent_id.as_deref(), Some(user1.as_str()));
        assert_eq!(branch.from_id, user1);
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(role_tag(&context.messages[1]), "branchSummary");
    }

    /// "persists compaction usage" (session.test.ts:98-121).
    async fn persists_compaction_usage<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        let first_kept_entry_id = session
            .append_message(user_message("one"))
            .await
            .expect("append");
        let usage = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 10,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
            },
        };
        let compaction_id = session
            .append_compaction(
                "summary",
                Some(&first_kept_entry_id),
                1234,
                AppendCompactionOptions {
                    from_hook: Some(false),
                    usage: Some(usage.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("append");
        let compaction_entry = session
            .get_entry(&compaction_id)
            .await
            .expect("entry")
            .expect("found");
        let SessionEntry::Compaction(compaction) = compaction_entry else {
            panic!("expected compaction entry");
        };
        assert_eq!(compaction.usage, Some(usage));
    }

    /// "persists branch summary usage" (session.test.ts:123-139).
    async fn persists_branch_summary_usage<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        let user1 = session
            .append_message(user_message("one"))
            .await
            .expect("append");
        let usage = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 10,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
            },
        };
        let summary_id = session
            .move_to(
                Some(&user1),
                Some(MoveToSummary {
                    summary: "summary text".to_owned(),
                    usage: Some(usage.clone()),
                    ..Default::default()
                }),
            )
            .await
            .expect("move")
            .expect("summary id");
        let summary_entry = session
            .get_entry(&summary_id)
            .await
            .expect("entry")
            .expect("found");
        let SessionEntry::BranchSummary(branch) = summary_entry else {
            panic!("expected branch_summary entry");
        };
        assert_eq!(branch.usage, Some(usage));
    }

    /// "supports custom message entries in context" (session.test.ts:141-147).
    async fn supports_custom_message_entries_in_context<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_custom_message_entry(
                "custom",
                UserContent::Text("hello".to_owned()),
                true,
                Some(json!({"ok": true})),
            )
            .await
            .expect("append");
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(role_tag(&context.messages[1]), "custom");
    }

    /// "keeps custom entries in context entries but omits them from messages by
    /// default" (session.test.ts:149-157).
    async fn keeps_custom_entries_in_context_entries_but_omits_them_from_messages_by_default<
        M: Send + Sync + 'static,
    >(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_custom_entry("chat_message", Some(json!({"text": "hello"})))
            .await
            .expect("append");
        let context_entries = session
            .build_context_entries(SessionContextBuildOptions::default())
            .await
            .expect("entries");
        let types: Vec<&str> = context_entries.iter().map(SessionEntry::type_tag).collect();
        assert_eq!(types, ["message", "custom"]);
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(context.messages.len(), 1);
    }

    /// "normalizes session names" (session.test.ts:188-192).
    async fn normalizes_session_names<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_session_name(" hello\nworld\r\nagain ")
            .await
            .expect("append");
        assert_eq!(
            session.get_session_name().await.expect("name").as_deref(),
            Some("hello world again")
        );
    }

    /// "supports labels and session info entries without affecting context"
    /// (session.test.ts:194-205).
    async fn supports_labels_and_session_info_entries_without_affecting_context<
        M: Send + Sync + 'static,
    >(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        let user1 = session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_label(&user1, Some("checkpoint"))
            .await
            .expect("append");
        session.append_session_name("name").await.expect("append");
        let entries = session
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries");
        assert!(
            entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::Label(_)))
        );
        assert!(
            entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::SessionInfo(_)))
        );
        assert_eq!(
            session.get_label(&user1).await.expect("label").as_deref(),
            Some("checkpoint")
        );
        assert_eq!(
            session.get_session_name().await.expect("name").as_deref(),
            Some("name")
        );
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(context.messages.len(), 1);
    }

    /// "rejects labels for missing entries" (session.test.ts:207-210).
    async fn rejects_labels_for_missing_entries<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        let error = session
            .append_label("missing", Some("checkpoint"))
            .await
            .expect_err("expected an error");
        assert_eq!(error.code, SessionErrorCode::NotFound);
        assert!(error.message.contains("Entry missing not found"));
    }

    /// Extra beyond session.test.ts (concurrency is not expressible in the
    /// single-threaded upstream test): concurrent appends on one facade
    /// instance must form an unforked chain. Without `with_append_lock`, the
    /// `createEntryId → getLeafId → appendEntry` sequences interleave and a
    /// later entry can parent on a stale leaf.
    async fn concurrent_appends_form_an_unforked_chain<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        const TASKS: usize = 64;
        let mut join_set = tokio::task::JoinSet::new();
        for _ in 0..TASKS {
            let session = Arc::clone(&session);
            join_set.spawn(async move {
                session
                    .append_message(user_message("m"))
                    .await
                    .expect("append")
            });
        }
        let mut ids: Vec<String> = Vec::with_capacity(TASKS);
        while let Some(result) = join_set.join_next().await {
            ids.push(result.expect("append task panicked"));
        }
        assert_eq!(ids.len(), TASKS);

        let entries = session
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries");
        assert_eq!(entries.len(), TASKS, "every append landed");
        let mut parents: HashMap<&str, Option<&str>> = HashMap::with_capacity(TASKS);
        for entry in &entries {
            parents.insert(entry.id(), entry.parent_id());
        }
        assert_eq!(
            parents.values().filter(|parent| parent.is_none()).count(),
            1,
            "exactly one root entry"
        );

        // Walk the chain from the leaf back to the root: every appended id
        // must be visited exactly once (a fork or a stale-leaf parent would
        // leave an id unvisited or the walk unrooted).
        let leaf = session
            .get_leaf_id()
            .await
            .expect("leaf")
            .expect("leaf must be set");
        let mut visited: Vec<&str> = Vec::with_capacity(TASKS);
        let mut current = Some(leaf.as_str());
        let mut hops = 0;
        while let Some(id) = current {
            visited.push(id);
            current = parents.get(id).copied().flatten();
            hops += 1;
            assert!(
                hops <= TASKS,
                "chain longer than the number of entries: cycle?"
            );
        }
        visited.sort_unstable();
        let mut expected: Vec<&str> = ids.iter().map(String::as_str).collect();
        expected.sort_unstable();
        assert_eq!(
            visited, expected,
            "chain must cover every appended entry exactly once"
        );
    }

    /// Extra beyond session.test.ts: `deriveSessionContextState`
    /// (session.ts:39-57) derives `activeToolNames` from
    /// `active_tools_change` entries and the model from assistant messages
    /// (provider/model), with `thinkingLevel` staying at the default.
    async fn tracks_active_tools_and_model_from_assistant_messages<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_active_tools_change(&["read".to_owned(), "bash".to_owned()])
            .await
            .expect("append");
        session
            .append_message(assistant_message("two"))
            .await
            .expect("append");
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(
            context.active_tool_names.as_deref(),
            Some(&["read".to_owned(), "bash".to_owned()][..])
        );
        assert_eq!(
            context.model,
            Some(SessionModelRef {
                provider: "anthropic".to_owned(),
                model_id: "claude-sonnet-4-5".to_owned(),
            })
        );
        assert_eq!(context.thinking_level, "off");
    }

    session_suite!(
        in_memory_appends_messages_and_builds_context_in_order,
        jsonl_appends_messages_and_builds_context_in_order,
        appends_messages_and_builds_context_in_order
    );
    session_suite!(
        in_memory_tracks_model_and_thinking_level_changes,
        jsonl_tracks_model_and_thinking_level_changes,
        tracks_model_and_thinking_level_changes
    );
    session_suite!(
        in_memory_supports_branching_by_moving_the_leaf_and_appending_a_new_branch,
        jsonl_supports_branching_by_moving_the_leaf_and_appending_a_new_branch,
        supports_branching_by_moving_the_leaf_and_appending_a_new_branch
    );
    session_suite!(
        in_memory_supports_moving_the_leaf_to_root,
        jsonl_supports_moving_the_leaf_to_root,
        supports_moving_the_leaf_to_root
    );
    session_suite!(
        in_memory_reconstructs_compaction_summaries_in_context,
        jsonl_reconstructs_compaction_summaries_in_context,
        reconstructs_compaction_summaries_in_context
    );
    session_suite!(
        in_memory_supports_moving_with_branch_summary_entries_in_context,
        jsonl_supports_moving_with_branch_summary_entries_in_context,
        supports_moving_with_branch_summary_entries_in_context
    );
    session_suite!(
        in_memory_persists_compaction_usage,
        jsonl_persists_compaction_usage,
        persists_compaction_usage
    );
    session_suite!(
        in_memory_persists_branch_summary_usage,
        jsonl_persists_branch_summary_usage,
        persists_branch_summary_usage
    );
    session_suite!(
        in_memory_supports_custom_message_entries_in_context,
        jsonl_supports_custom_message_entries_in_context,
        supports_custom_message_entries_in_context
    );
    session_suite!(
        in_memory_keeps_custom_entries_in_context_entries_but_omits_them_from_messages_by_default,
        jsonl_keeps_custom_entries_in_context_entries_but_omits_them_from_messages_by_default,
        keeps_custom_entries_in_context_entries_but_omits_them_from_messages_by_default
    );
    session_suite!(
        in_memory_normalizes_session_names,
        jsonl_normalizes_session_names,
        normalizes_session_names
    );
    session_suite!(
        in_memory_supports_labels_and_session_info_entries_without_affecting_context,
        jsonl_supports_labels_and_session_info_entries_without_affecting_context,
        supports_labels_and_session_info_entries_without_affecting_context
    );
    session_suite!(
        in_memory_rejects_labels_for_missing_entries,
        jsonl_rejects_labels_for_missing_entries,
        rejects_labels_for_missing_entries
    );
    session_suite!(
        in_memory_concurrent_appends_form_an_unforked_chain,
        jsonl_concurrent_appends_form_an_unforked_chain,
        concurrent_appends_form_an_unforked_chain
    );
    session_suite!(
        in_memory_tracks_active_tools_and_model_from_assistant_messages,
        jsonl_tracks_active_tools_and_model_from_assistant_messages,
        tracks_active_tools_and_model_from_assistant_messages
    );

    /// "projects custom entries with configured custom-entry projectors"
    /// (session.test.ts:159-170) — needs a facade built with `entryProjectors`.
    fn projector_options() -> SessionContextBuildOptions {
        let projector: CustomEntryContextMessageProjector = Arc::new(
            |entry: &CustomEntry, _index: usize, _entries: &[SessionEntry]| -> Vec<AgentMessage> {
                vec![user_message(&format!(
                    "chat: {}",
                    get_text_data(entry.data.as_ref())
                ))]
            },
        );
        SessionContextBuildOptions {
            entry_projectors: HashMap::from([("chat_message".to_owned(), projector)]),
            ..Default::default()
        }
    }

    async fn projects_custom_entries_with_configured_custom_entry_projectors<
        M: Send + Sync + 'static,
    >(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_custom_entry("chat_message", Some(json!({"text": "hello"})))
            .await
            .expect("append");
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(roles(&context), ["user", "user"]);
        let AgentMessage::User(user) = &context.messages[1] else {
            panic!("expected a user message");
        };
        match &user.content {
            UserContent::Text(text) => assert_eq!(text, "chat: hello"),
            UserContent::Blocks(_) => panic!("expected text content"),
        }
    }

    #[tokio::test]
    async fn in_memory_projects_custom_entries_with_configured_custom_entry_projectors() {
        projects_custom_entries_with_configured_custom_entry_projectors(build_session(
            memory_storage(),
            projector_options(),
        ))
        .await;
    }

    #[tokio::test]
    async fn jsonl_projects_custom_entries_with_configured_custom_entry_projectors() {
        let fs = TestFs::new();
        projects_custom_entries_with_configured_custom_entry_projectors(build_session(
            jsonl_storage(fs.clone()).await,
            projector_options(),
        ))
        .await;
    }

    /// "applies context entry transforms after default compaction selection"
    /// (session.test.ts:172-186) — the transform observes the default-compacted
    /// entry list (first entry is the compaction) and drops compaction entries.
    fn drop_compaction_options(
        observed_first_entry_type: &Arc<std::sync::Mutex<Option<String>>>,
    ) -> SessionContextBuildOptions {
        let observed = Arc::clone(observed_first_entry_type);
        let drop_compaction: ContextEntryTransform = Arc::new(move |entries: &[SessionEntry]| {
            *observed.lock().unwrap_or_else(|p| p.into_inner()) =
                entries.first().map(|entry| entry.type_tag().to_owned());
            entries
                .iter()
                .filter(|entry| !matches!(entry, SessionEntry::Compaction(_)))
                .cloned()
                .collect()
        });
        SessionContextBuildOptions {
            entry_transforms: vec![drop_compaction],
            ..Default::default()
        }
    }

    async fn applies_context_entry_transforms_after_default_compaction_selection<
        M: Send + Sync + 'static,
    >(
        session: Arc<dyn SessionTrait<Metadata = M>>,
        observed_first_entry_type: Arc<std::sync::Mutex<Option<String>>>,
    ) {
        session
            .append_message(user_message("one"))
            .await
            .expect("append");
        let kept = session
            .append_message(user_message("two"))
            .await
            .expect("append");
        session
            .append_compaction(
                "summary",
                Some(&kept),
                1234,
                AppendCompactionOptions::default(),
            )
            .await
            .expect("append");
        session
            .append_message(user_message("three"))
            .await
            .expect("append");
        let context = session
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(
            observed_first_entry_type
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_deref(),
            Some("compaction")
        );
        assert_eq!(roles(&context), ["user", "user"]);
    }

    #[tokio::test]
    async fn in_memory_applies_context_entry_transforms_after_default_compaction_selection() {
        let observed = Arc::new(std::sync::Mutex::new(None));
        applies_context_entry_transforms_after_default_compaction_selection(
            build_session(memory_storage(), drop_compaction_options(&observed)),
            observed,
        )
        .await;
    }

    #[tokio::test]
    async fn jsonl_applies_context_entry_transforms_after_default_compaction_selection() {
        let fs = TestFs::new();
        let observed = Arc::new(std::sync::Mutex::new(None));
        applies_context_entry_transforms_after_default_compaction_selection(
            build_session(
                jsonl_storage(fs.clone()).await,
                drop_compaction_options(&observed),
            ),
            observed,
        )
        .await;
    }

    /// "persists leaf changes and appended entries via storage"
    /// (session.test.ts:212-227) — a second facade over the same storage sees
    /// the appended entries; the JSONL wrapper additionally runs the suite's
    /// `inspect` callback on the session file.
    async fn persists_leaf_changes_and_appended_entries_via_storage<M: Send + Sync + 'static>(
        session: Arc<dyn SessionTrait<Metadata = M>>,
    ) {
        let user1 = session
            .append_message(user_message("one"))
            .await
            .expect("append");
        session
            .append_message(assistant_message("two"))
            .await
            .expect("append");
        session
            .append_label(&user1, Some("checkpoint"))
            .await
            .expect("append");
        session.append_session_name("name").await.expect("append");
        session.move_to(Some(&user1), None).await.expect("move");
        session
            .append_message(assistant_message("branched"))
            .await
            .expect("append");
        let session2 = build_session(session.storage(), SessionContextBuildOptions::default());
        let context = session2
            .build_context(SessionContextBuildOptions::default())
            .await
            .expect("context");
        assert_eq!(roles(&context), ["user", "assistant"]);
        assert_eq!(
            session2.get_label(&user1).await.expect("label").as_deref(),
            Some("checkpoint")
        );
        assert_eq!(
            session2.get_session_name().await.expect("name").as_deref(),
            Some("name")
        );
    }

    #[tokio::test]
    async fn in_memory_persists_leaf_changes_and_appended_entries_via_storage() {
        persists_leaf_changes_and_appended_entries_via_storage(memory_session()).await;
    }

    #[tokio::test]
    async fn jsonl_persists_leaf_changes_and_appended_entries_via_storage() {
        let fs = TestFs::new();
        persists_leaf_changes_and_appended_entries_via_storage(jsonl_session(fs.clone()).await)
            .await;
        inspect_jsonl_file(&fs);
    }
}
