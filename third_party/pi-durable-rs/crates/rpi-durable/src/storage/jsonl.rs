//! Portable JSONL implementation of the storage contract
//! (port of `src/storage/jsonl/storage.ts` @ `abe508e1b`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::chord::context::Context;
use crate::chord::json::JsonObject;
use crate::errors::{StorageError, StorageResult};
use crate::fs::{DirEntryKind, FileSystem, error_from_file};
use crate::ids::{ConversationId, DocumentId, EntryId, Seq, SubmissionId, TaskId, id_from_number};
use crate::storage::is_current_only_create;
use crate::storage::memory::{MemoryStorage, PreparedMemoryCommit};
use crate::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent,
    DocumentCreate, DocumentPoint, DocumentQuery, DocumentRecord, EntryQuery, EntryRecord, Page,
    Storage, StorageWrite, StoredDocument, SubmissionQuery, SubmissionRecord, TaskQuery,
    TaskRecord,
};

const FORMAT_VERSION: u64 = 1;
const MAIN_FILE: &str = "main.jsonl";
const RECLAIM_SUFFIX: &str = ".reclaim";

/// JSONL publication options.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonlStorageOptions {
    /// Flush every affected sidecar before appending the main marker. Defaults
    /// to false (upstream `JsonlStorageOptions.fsync`).
    pub fsync: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MainMarker {
    format: u64,
    #[serde(rename = "type")]
    kind: String,
    seq: Seq,
    writes: Vec<MainOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)] // persisted wire shape; boxing would change it
enum MainOperation {
    #[serde(rename = "conversation")]
    Conversation { value: ConversationRecord },
    #[serde(rename = "entry")]
    Entry { value: EntryRecord },
    #[serde(rename = "submission")]
    Submission { value: SubmissionRecord },
    #[serde(rename = "task")]
    Task { value: TaskRecord },
    #[serde(rename = "task.sidecar")]
    TaskSidecar { id: TaskId, ordinal: u64 },
    #[serde(rename = "document.create")]
    DocumentCreate {
        record: DocumentCreate,
        ordinal: u64,
    },
    #[serde(rename = "document.change")]
    DocumentChange { id: DocumentId, ordinal: u64 },
    #[serde(rename = "document.retire")]
    DocumentRetire { id: DocumentId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SidecarRecord {
    format: u64,
    #[serde(rename = "type")]
    kind: String,
    seq: Seq,
    ordinal: u64,
    payload: SidecarPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)] // persisted wire shape; boxing would change it
enum SidecarPayload {
    #[serde(rename = "task")]
    Task { value: TaskRecord },
    #[serde(rename = "document")]
    Document {
        id: DocumentId,
        content: DocumentContent,
    },
}

#[derive(Debug, Clone)]
struct ParsedLine<T> {
    value: T,
    start: u64,
}

#[derive(Debug, Clone)]
struct ParsedFile<T> {
    path: String,
    lines: Vec<ParsedLine<T>>,
}

#[derive(Debug, Default)]
struct EncodedCommit {
    marker: String,
    sidecars: Vec<(String, String)>,
}

fn sidecar_file_name(kind: &str, id: u64) -> String {
    format!("{kind}-{id}.jsonl")
}

fn is_sidecar_file_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("doc-")
        .or_else(|| name.strip_prefix("task-"))
        .and_then(|rest| rest.strip_suffix(".jsonl"))
    else {
        return false;
    };
    !rest.is_empty()
        && rest.bytes().all(|byte| byte.is_ascii_digit())
        && (rest == "0" || !rest.starts_with('0'))
}

fn is_reclaim_file_name(name: &str) -> bool {
    name.strip_suffix(RECLAIM_SUFFIX)
        .is_some_and(is_sidecar_file_name)
}

fn sidecar_key(file: &str, seq: Seq, ordinal: u64) -> String {
    serde_json::to_string(&serde_json::json!([file, seq.0, ordinal])).unwrap_or_default()
}

fn json_line<T: Serialize>(value: &T) -> StorageResult<String> {
    let mut text = serde_json::to_string(value)?;
    text.push('\n');
    Ok(text)
}

fn corruption(message: impl Into<String>) -> StorageError {
    StorageError::JsonlCorruption(message.into())
}

fn parse_json(text: &str, description: &str) -> StorageResult<serde_json::Value> {
    serde_json::from_str(text)
        .map_err(|error| corruption(format!("Malformed complete {description}: {error}")))
}

fn parse_main_marker(text: &str, line: u64) -> StorageResult<MainMarker> {
    let description = format!("{MAIN_FILE} line {line}");
    let value = parse_json(text, &description)?;
    let marker: MainMarker = serde_json::from_value(value)
        .map_err(|error| corruption(format!("Invalid commit marker in {description}: {error}")))?;
    if marker.format != FORMAT_VERSION || marker.kind != "commit" || marker.seq.0 < 1 {
        return Err(corruption(format!(
            "Invalid commit marker in {description}"
        )));
    }
    for write in &marker.writes {
        match write {
            MainOperation::Conversation { .. }
            | MainOperation::Entry { .. }
            | MainOperation::Submission { .. } => {}
            MainOperation::Task { value } => {
                if !matches!(value.state, crate::types::TaskState::Terminal { .. }) {
                    return Err(corruption(format!(
                        "Invalid terminal task write in {description}"
                    )));
                }
            }
            MainOperation::TaskSidecar { ordinal, .. }
            | MainOperation::DocumentCreate { ordinal, .. } => {
                if *ordinal > i64::MAX as u64 {
                    return Err(corruption(format!(
                        "Invalid sidecar write in {description}"
                    )));
                }
            }
            MainOperation::DocumentChange { ordinal, .. } => {
                if *ordinal > i64::MAX as u64 {
                    return Err(corruption(format!(
                        "Invalid document change in {description}"
                    )));
                }
            }
            MainOperation::DocumentRetire { .. } => {}
        }
    }
    Ok(marker)
}

fn validate_document_content(content: &DocumentContent, description: &str) -> StorageResult<()> {
    if content.version() < 1 {
        return Err(corruption(format!(
            "Invalid document content in {description}"
        )));
    }
    Ok(())
}

fn parse_sidecar_record(text: &str, file: &str, line: u64) -> StorageResult<SidecarRecord> {
    let description = format!("{file} line {line}");
    let value = parse_json(text, &description)?;
    let record: SidecarRecord = serde_json::from_value(value)
        .map_err(|error| corruption(format!("Invalid sidecar record in {description}: {error}")))?;
    if record.format != FORMAT_VERSION || record.kind != "record" || record.seq.0 < 1 {
        return Err(corruption(format!(
            "Invalid sidecar record in {description}"
        )));
    }
    match &record.payload {
        SidecarPayload::Task { value } => {
            if matches!(value.state, crate::types::TaskState::Terminal { .. }) {
                return Err(corruption(format!(
                    "Invalid live task record in {description}"
                )));
            }
        }
        SidecarPayload::Document { content, .. } => {
            validate_document_content(content, &description)?
        }
    }
    Ok(record)
}

fn is_current_only(record: &DocumentCreate) -> bool {
    is_current_only_create(record)
}

/// Portable JSONL implementation of the storage contract.
pub struct JsonlStorage {
    fs: Arc<dyn FileSystem>,
    directory: String,
    main_path: String,
    fsync: bool,
    memory: MemoryStorage,
    current_only_documents: Mutex<HashSet<DocumentId>>,
    live_task_sidecars: Mutex<HashSet<TaskId>>,
    closed: AtomicBool,
    poison_error: Mutex<Option<String>>,
}

impl JsonlStorage {
    /// Open or create a JSONL storage directory using the supplied filesystem.
    pub async fn open(
        directory: &str,
        fs: Arc<dyn FileSystem>,
        options: JsonlStorageOptions,
        context: &Context,
    ) -> StorageResult<Self> {
        let absolute = fs
            .absolute_path(directory, context)
            .await
            .map_err(|error| error_from_file("path resolution", error))?;
        fs.create_dir(&absolute, true, context)
            .await
            .map_err(|error| error_from_file("directory creation", error))?;
        let main_path = fs
            .join_path(&[absolute.as_str(), MAIN_FILE], context)
            .await
            .map_err(|error| error_from_file("path join", error))?;
        let storage = Self {
            fs,
            directory: absolute,
            main_path,
            fsync: options.fsync,
            memory: MemoryStorage::new(),
            current_only_documents: Mutex::new(HashSet::new()),
            live_task_sidecars: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
            poison_error: Mutex::new(None),
        };
        storage.recover(context).await?;
        Ok(storage)
    }

    /// Flush-on-commit option.
    pub fn fsync(&self) -> bool {
        self.fsync
    }

    fn assert_usable(&self) -> StorageResult<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(StorageError::Closed("JsonlStorage"));
        }
        if let Some(cause) = self
            .poison_error
            .lock()
            .map_err(|_| StorageError::message("poison lock"))?
            .clone()
        {
            return Err(StorageError::JsonlPoisoned(cause));
        }
        Ok(())
    }

    fn poison(&self, cause: StorageError) -> StorageError {
        let mut slot = match self.poison_error.lock() {
            Ok(slot) => slot,
            Err(_) => return StorageError::message("poison lock"),
        };
        if slot.is_none() {
            *slot = Some(cause.to_string());
        }
        StorageError::JsonlPoisoned(cause.to_string())
    }

    fn encode_commit(&self, seq: Seq, writes: &[StorageWrite]) -> StorageResult<EncodedCommit> {
        let mut main_writes: Vec<MainOperation> = Vec::new();
        let mut records: Vec<(String, Vec<SidecarRecord>)> = Vec::new();
        let mut next_ordinal: u64 = 0;
        let add_sidecar = |records: &mut Vec<(String, Vec<SidecarRecord>)>,
                           next_ordinal: &mut u64,
                           file: String,
                           payload: SidecarPayload|
         -> u64 {
            let ordinal = *next_ordinal;
            *next_ordinal += 1;
            let record = SidecarRecord {
                format: FORMAT_VERSION,
                kind: "record".to_string(),
                seq,
                ordinal,
                payload,
            };
            if let Some(entry) = records.iter_mut().find(|(name, _)| name == &file) {
                entry.1.push(record);
            } else {
                records.push((file, vec![record]));
            }
            ordinal
        };

        for write in writes {
            match write {
                StorageWrite::Conversation { .. }
                | StorageWrite::Entry { .. }
                | StorageWrite::Submission { .. }
                | StorageWrite::DocumentRetire { .. } => main_writes.push(match write {
                    StorageWrite::Conversation { value } => MainOperation::Conversation {
                        value: value.clone(),
                    },
                    StorageWrite::Entry { value } => MainOperation::Entry {
                        value: value.clone(),
                    },
                    StorageWrite::Submission { value } => MainOperation::Submission {
                        value: value.clone(),
                    },
                    StorageWrite::DocumentRetire { id } => {
                        MainOperation::DocumentRetire { id: *id }
                    }
                    _ => {
                        return Err(StorageError::message(
                            "unsupported write variant in the main-record branch",
                        ));
                    }
                }),
                StorageWrite::Task { value } => {
                    if matches!(value.state, crate::types::TaskState::Terminal { .. }) {
                        main_writes.push(MainOperation::Task {
                            value: value.clone(),
                        });
                    } else {
                        let ordinal = add_sidecar(
                            &mut records,
                            &mut next_ordinal,
                            sidecar_file_name("task", value.id.get()),
                            SidecarPayload::Task {
                                value: value.clone(),
                            },
                        );
                        main_writes.push(MainOperation::TaskSidecar {
                            id: value.id,
                            ordinal,
                        });
                    }
                }
                StorageWrite::DocumentCreate { record, content } => {
                    let ordinal = add_sidecar(
                        &mut records,
                        &mut next_ordinal,
                        sidecar_file_name("doc", record.id.get()),
                        SidecarPayload::Document {
                            id: record.id,
                            content: content.clone(),
                        },
                    );
                    main_writes.push(MainOperation::DocumentCreate {
                        record: record.clone(),
                        ordinal,
                    });
                }
                StorageWrite::DocumentChange { id, content } => {
                    let ordinal = add_sidecar(
                        &mut records,
                        &mut next_ordinal,
                        sidecar_file_name("doc", id.get()),
                        SidecarPayload::Document {
                            id: *id,
                            content: content.clone(),
                        },
                    );
                    main_writes.push(MainOperation::DocumentChange { id: *id, ordinal });
                }
                StorageWrite::DocumentCopy { .. } => {
                    return Err(StorageError::message(
                        "Prepared document copy was not resolved",
                    ));
                }
            }
        }

        let sidecars = records
            .into_iter()
            .map(|(file, file_records)| {
                let mut content = String::new();
                for record in &file_records {
                    content.push_str(&json_line(record)?);
                }
                Ok((file, content))
            })
            .collect::<StorageResult<Vec<_>>>()?;
        let marker = json_line(&MainMarker {
            format: FORMAT_VERSION,
            kind: "commit".to_string(),
            seq,
            writes: main_writes,
        })?;
        Ok(EncodedCommit { marker, sidecars })
    }

    fn plan_reclamations(
        &self,
        writes: &[StorageWrite],
        encoded: &EncodedCommit,
    ) -> StorageResult<Vec<(String, String)>> {
        let mut created_current_only: HashSet<DocumentId> = HashSet::new();
        let mut retired_documents: HashSet<DocumentId> = HashSet::new();
        let mut base_documents: HashSet<DocumentId> = HashSet::new();
        let mut final_tasks: HashMap<TaskId, &TaskRecord> = HashMap::new();
        for write in writes {
            match write {
                StorageWrite::DocumentCreate { record, .. } => {
                    if is_current_only(record) {
                        created_current_only.insert(record.id);
                    }
                }
                StorageWrite::DocumentChange { id, content } => {
                    if content.is_base() {
                        base_documents.insert(*id);
                    }
                }
                StorageWrite::DocumentRetire { id } => {
                    retired_documents.insert(*id);
                }
                StorageWrite::Task { value } => {
                    final_tasks.insert(value.id, value);
                }
                _ => {}
            }
        }

        let current_only = self
            .current_only_documents
            .lock()
            .map_err(|_| StorageError::message("sidecar lock"))?
            .clone();
        let live_task_sidecars = self
            .live_task_sidecars
            .lock()
            .map_err(|_| StorageError::message("sidecar lock"))?
            .clone();
        let is_current_only_document =
            |id: &DocumentId| current_only.contains(id) || created_current_only.contains(id);

        let mut replacements: Vec<(String, String)> = Vec::new();
        for id in &retired_documents {
            if is_current_only_document(id) {
                replacements.push((sidecar_file_name("doc", id.get()), String::new()));
            }
        }
        for id in &base_documents {
            if !is_current_only_document(id) || retired_documents.contains(id) {
                continue;
            }
            let file = sidecar_file_name("doc", id.get());
            if let Some((_, content)) = encoded.sidecars.iter().find(|(name, _)| name == &file) {
                replacements.push((file, content.clone()));
            }
        }
        for (id, task) in final_tasks {
            if matches!(task.state, crate::types::TaskState::Terminal { .. }) {
                let file = sidecar_file_name("task", id.get());
                if live_task_sidecars.contains(&id)
                    || encoded.sidecars.iter().any(|(name, _)| name == &file)
                {
                    replacements.push((file, String::new()));
                }
            }
        }
        Ok(replacements)
    }

    fn adopt_sidecar_state(&self, writes: &[StorageWrite]) -> StorageResult<()> {
        let mut current_only = self
            .current_only_documents
            .lock()
            .map_err(|_| StorageError::message("sidecar lock"))?;
        let mut live_tasks = self
            .live_task_sidecars
            .lock()
            .map_err(|_| StorageError::message("sidecar lock"))?;
        for write in writes {
            match write {
                StorageWrite::DocumentCreate { record, .. } => {
                    if is_current_only(record) {
                        current_only.insert(record.id);
                    }
                }
                StorageWrite::Task { value } => {
                    if matches!(value.state, crate::types::TaskState::Terminal { .. }) {
                        live_tasks.remove(&value.id);
                    } else {
                        live_tasks.insert(value.id);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The marker already published this state, so reclamation is retryable
    /// best-effort maintenance.
    async fn reclaim_sidecars(&self, replacements: &[(String, String)], context: &Context) {
        if replacements.is_empty() {
            return;
        }
        if self.fsync && self.fs.flush_file(&self.main_path, context).await.is_err() {
            return;
        }
        for (file, content) in replacements {
            self.replace_sidecar(file, content, context).await;
        }
    }

    async fn replace_sidecar(&self, file: &str, content: &str, context: &Context) {
        let Ok(path) = self
            .fs
            .join_path(&[self.directory.as_str(), file], context)
            .await
        else {
            return;
        };
        if content.is_empty() {
            let _ = self.fs.remove(&path, true, context).await;
            return;
        }
        let temporary = format!("{file}{RECLAIM_SUFFIX}");
        let Ok(temporary_path) = self
            .fs
            .join_path(&[self.directory.as_str(), temporary.as_str()], context)
            .await
        else {
            return;
        };
        if self
            .fs
            .write_file(&temporary_path, content, context)
            .await
            .is_err()
        {
            return;
        }
        if self.fsync && self.fs.flush_file(&temporary_path, context).await.is_err() {
            return;
        }
        let _ = self.fs.rename_file(&temporary_path, &path, context).await;
    }

    async fn recover(&self, context: &Context) -> StorageResult<()> {
        let main = self
            .read_lines(&self.main_path, MAIN_FILE, context, parse_main_marker)
            .await?;
        let mut previous_seq = 0u64;
        for marker in &main.lines {
            if marker.value.seq.0 <= previous_seq {
                return Err(corruption(format!(
                    "Commit sequence does not strictly increase in {MAIN_FILE}"
                )));
            }
            previous_seq = marker.value.seq.0;
        }

        let listed = self
            .fs
            .list_dir(&self.directory, context)
            .await
            .map_err(|error| error_from_file("directory listing", error))?;
        for info in &listed {
            if info.kind == DirEntryKind::File && is_reclaim_file_name(&info.name) {
                let _ = self.fs.remove(&info.path, true, context).await;
            }
        }
        let mut sidecar_files: Vec<String> = listed
            .iter()
            .filter(|info| info.kind == DirEntryKind::File && is_sidecar_file_name(&info.name))
            .map(|info| info.name.clone())
            .collect();
        sidecar_files.sort();

        let mut parsed_files: Vec<ParsedFile<SidecarRecord>> = Vec::new();
        let mut record_by_key: HashMap<String, ParsedLine<SidecarRecord>> = HashMap::new();
        for file in &sidecar_files {
            let path = self
                .fs
                .join_path(&[self.directory.as_str(), file.as_str()], context)
                .await
                .map_err(|error| error_from_file("path join", error))?;
            let parsed = self
                .read_lines(&path, file, context, {
                    let file = file.clone();
                    move |text, line| parse_sidecar_record(text, &file, line)
                })
                .await?;
            let mut previous: Option<&SidecarRecord> = None;
            for line in &parsed.lines {
                if let Some(previous) = previous
                    && (line.value.seq < previous.seq
                        || (line.value.seq == previous.seq
                            && line.value.ordinal <= previous.ordinal))
                {
                    return Err(corruption(format!(
                        "Sidecar records are out of order in {file}"
                    )));
                }
                previous = Some(&line.value);
                record_by_key.insert(
                    sidecar_key(file, line.value.seq, line.value.ordinal),
                    line.clone(),
                );
            }
            parsed_files.push(parsed);
        }

        let mut current_only_documents: HashSet<DocumentId> = HashSet::new();
        let mut retired_documents: HashSet<DocumentId> = HashSet::new();
        let mut final_task_is_live: HashMap<TaskId, bool> = HashMap::new();
        for line in &main.lines {
            for operation in &line.value.writes {
                match operation {
                    MainOperation::DocumentCreate { record, .. } => {
                        if is_current_only(record) {
                            current_only_documents.insert(record.id);
                        }
                    }
                    MainOperation::DocumentRetire { id } => {
                        retired_documents.insert(*id);
                    }
                    MainOperation::Task { value } => {
                        final_task_is_live.insert(value.id, false);
                    }
                    MainOperation::TaskSidecar { id, .. } => {
                        final_task_is_live.insert(*id, true);
                    }
                    _ => {}
                }
            }
        }
        let retired_current_only: HashSet<DocumentId> = retired_documents
            .iter()
            .filter(|id| current_only_documents.contains(id))
            .copied()
            .collect();

        let mut latest_bases: HashMap<DocumentId, SidecarRecord> = HashMap::new();
        for line in &main.lines {
            for operation in &line.value.writes {
                let (id, ordinal) = match operation {
                    MainOperation::DocumentCreate { record, ordinal } => (record.id, *ordinal),
                    MainOperation::DocumentChange { id, ordinal } => (*id, *ordinal),
                    _ => continue,
                };
                if !current_only_documents.contains(&id) {
                    continue;
                }
                let Some(record) = record_by_key
                    .get(&sidecar_key(
                        &sidecar_file_name("doc", id.get()),
                        line.value.seq,
                        ordinal,
                    ))
                    .map(|line| line.value.clone())
                else {
                    continue;
                };
                let SidecarPayload::Document {
                    id: payload_id,
                    content,
                } = &record.payload
                else {
                    continue;
                };
                if *payload_id != id || !content.is_base() {
                    continue;
                }
                let replace = latest_bases.get(&id).is_none_or(|previous| {
                    record.seq > previous.seq
                        || (record.seq == previous.seq && record.ordinal > previous.ordinal)
                });
                if replace {
                    latest_bases.insert(id, record);
                }
            }
        }
        let is_before_latest_base = |id: DocumentId, seq: Seq, ordinal: u64| -> bool {
            latest_bases
                .get(&id)
                .is_some_and(|base| seq < base.seq || (seq == base.seq && ordinal < base.ordinal))
        };
        let terminal_tasks: HashSet<TaskId> = final_task_is_live
            .iter()
            .filter(|(_, live)| !**live)
            .map(|(id, _)| *id)
            .collect();
        let mut confirmed: HashSet<String> = HashSet::new();

        for line in &main.lines {
            let marker = &line.value;
            let mut writes: Vec<StorageWrite> = Vec::new();
            for operation in &marker.writes {
                match operation {
                    MainOperation::Conversation { value } => {
                        writes.push(StorageWrite::Conversation {
                            value: value.clone(),
                        })
                    }
                    MainOperation::Entry { value } => writes.push(StorageWrite::Entry {
                        value: value.clone(),
                    }),
                    MainOperation::Submission { value } => writes.push(StorageWrite::Submission {
                        value: value.clone(),
                    }),
                    MainOperation::Task { value } => writes.push(StorageWrite::Task {
                        value: value.clone(),
                    }),
                    MainOperation::DocumentRetire { id } => {
                        writes.push(StorageWrite::DocumentRetire { id: *id })
                    }
                    MainOperation::TaskSidecar { id, ordinal } => {
                        let optional = terminal_tasks.contains(id);
                        let record = Self::confirm_record(
                            marker.seq,
                            *ordinal,
                            &sidecar_file_name("task", id.get()),
                            &record_by_key,
                            &mut confirmed,
                            optional,
                        )?;
                        if let Some(record) = record {
                            let SidecarPayload::Task { value } = &record.payload else {
                                return Err(corruption(format!(
                                    "Confirmed task sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            };
                            if value.id != *id {
                                return Err(corruption(format!(
                                    "Confirmed task sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            }
                            if !optional {
                                writes.push(StorageWrite::Task {
                                    value: value.clone(),
                                });
                            }
                        }
                    }
                    MainOperation::DocumentCreate { record, ordinal } => {
                        let id = record.id;
                        let reclaimed = retired_current_only.contains(&id)
                            || is_before_latest_base(id, marker.seq, *ordinal);
                        let confirm = Self::confirm_record(
                            marker.seq,
                            *ordinal,
                            &sidecar_file_name("doc", id.get()),
                            &record_by_key,
                            &mut confirmed,
                            reclaimed,
                        )?;
                        let mut content: Option<DocumentContent> = None;
                        if let Some(record) = confirm {
                            let SidecarPayload::Document {
                                id: payload_id,
                                content: payload_content,
                            } = &record.payload
                            else {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            };
                            if *payload_id != id {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            }
                            content = Some(payload_content.clone());
                        }
                        if let Some(content) = &content
                            && !content.is_base()
                        {
                            return Err(corruption(format!(
                                "Document creation lacks a confirmed base in commit {}",
                                marker.seq
                            )));
                        }
                        let content = match (reclaimed, content) {
                            (false, Some(content)) => content,
                            _ => DocumentContent::Base {
                                version: 1,
                                value: JsonObject::new(),
                            },
                        };
                        writes.push(StorageWrite::DocumentCreate {
                            record: record.clone(),
                            content,
                        });
                    }
                    MainOperation::DocumentChange { id, ordinal } => {
                        let reclaimed = retired_current_only.contains(id)
                            || is_before_latest_base(*id, marker.seq, *ordinal);
                        let confirm = Self::confirm_record(
                            marker.seq,
                            *ordinal,
                            &sidecar_file_name("doc", id.get()),
                            &record_by_key,
                            &mut confirmed,
                            reclaimed,
                        )?;
                        let mut content: Option<DocumentContent> = None;
                        if let Some(record) = confirm {
                            let SidecarPayload::Document {
                                id: payload_id,
                                content: payload_content,
                            } = &record.payload
                            else {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            };
                            if *payload_id != *id {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            }
                            content = Some(payload_content.clone());
                        }
                        if !reclaimed && let Some(content) = content {
                            writes.push(StorageWrite::DocumentChange { id: *id, content });
                        }
                    }
                }
            }
            let mut prepared = self
                .memory
                .prepare_commit(writes, Some(marker.seq))
                .map_err(|error| {
                    corruption(format!(
                        "Invalid committed state at sequence {}: {error}",
                        marker.seq
                    ))
                })?;
            self.memory.apply_prepared(&mut prepared).map_err(|error| {
                corruption(format!(
                    "Invalid committed state at sequence {}: {error}",
                    marker.seq
                ))
            })?;
        }

        let mut reclamations: Vec<(String, String)> = Vec::new();
        for parsed in &parsed_files {
            let file = parsed
                .path
                .rsplit(std::path::MAIN_SEPARATOR)
                .next()
                .unwrap_or_default()
                .to_string();
            let file = if file.is_empty() {
                parsed.path.clone()
            } else {
                file
            };
            let mut unconfirmed_at: Option<u64> = None;
            for line in &parsed.lines {
                let key = sidecar_key(&file, line.value.seq, line.value.ordinal);
                if confirmed.contains(&key) {
                    if unconfirmed_at.is_some() {
                        return Err(corruption(format!(
                            "Confirmed record follows an unconfirmed tail in {file}"
                        )));
                    }
                } else if unconfirmed_at.is_none() {
                    unconfirmed_at = Some(line.start);
                }
            }
            if let Some(offset) = unconfirmed_at {
                self.fs
                    .truncate_file(&parsed.path, offset, context)
                    .await
                    .map_err(|error| {
                        error_from_file(&format!("tail truncation of {file}"), error)
                    })?;
            }

            let Some(numeric_id) = file
                .split_once('-')
                .and_then(|(_, rest)| rest.strip_suffix(".jsonl"))
                .and_then(|rest| rest.parse::<u64>().ok())
            else {
                continue;
            };
            let confirmed_lines: Vec<&ParsedLine<SidecarRecord>> = parsed
                .lines
                .iter()
                .filter(|line| {
                    confirmed.contains(&sidecar_key(&file, line.value.seq, line.value.ordinal))
                })
                .collect();
            let mut retained: Option<Vec<&ParsedLine<SidecarRecord>>> = None;
            if file.starts_with("task-")
                && terminal_tasks.contains(&id_from_number::<crate::ids::Task>(numeric_id))
            {
                retained = Some(Vec::new());
            } else if file.starts_with("doc-") {
                let document_id = id_from_number::<crate::ids::Document>(numeric_id);
                if retired_current_only.contains(&document_id) {
                    retained = Some(Vec::new());
                } else if latest_bases.contains_key(&document_id) {
                    retained = Some(
                        confirmed_lines
                            .iter()
                            .filter(|line| {
                                !is_before_latest_base(
                                    document_id,
                                    line.value.seq,
                                    line.value.ordinal,
                                )
                            })
                            .copied()
                            .collect(),
                    );
                }
            }
            if let Some(retained) = retained
                && (retained.len() < confirmed_lines.len() || retained.is_empty())
            {
                let mut content = String::new();
                for line in &retained {
                    content.push_str(&json_line(&line.value)?);
                }
                reclamations.push((file, content));
            }
        }
        self.reclaim_sidecars(&reclamations, context).await;

        {
            let mut current_only = self
                .current_only_documents
                .lock()
                .map_err(|_| StorageError::message("sidecar lock"))?;
            for id in current_only_documents {
                current_only.insert(id);
            }
        }
        {
            let mut live = self
                .live_task_sidecars
                .lock()
                .map_err(|_| StorageError::message("sidecar lock"))?;
            for (id, is_live) in final_task_is_live {
                if is_live {
                    live.insert(id);
                }
            }
        }
        Ok(())
    }

    fn confirm_record(
        marker_seq: Seq,
        ordinal: u64,
        file: &str,
        record_by_key: &HashMap<String, ParsedLine<SidecarRecord>>,
        confirmed: &mut HashSet<String>,
        optional: bool,
    ) -> StorageResult<Option<SidecarRecord>> {
        let key = sidecar_key(file, marker_seq, ordinal);
        if confirmed.contains(&key) {
            return Err(corruption("Sidecar record is confirmed more than once"));
        }
        let Some(line) = record_by_key.get(&key) else {
            if optional {
                return Ok(None);
            }
            return Err(corruption(format!(
                "Missing confirmed sidecar record {file} at sequence {marker_seq}"
            )));
        };
        confirmed.insert(key);
        Ok(Some(line.value.clone()))
    }

    async fn read_lines<T>(
        &self,
        path: &str,
        name: &str,
        context: &Context,
        parse: impl Fn(&str, u64) -> StorageResult<T>,
    ) -> StorageResult<ParsedFile<T>> {
        let bytes = match self.fs.read_binary_file(path, context).await {
            Ok(bytes) => bytes,
            Err(error) if error.code == crate::errors::FileErrorCode::NotFound => {
                return Ok(ParsedFile {
                    path: path.to_string(),
                    lines: Vec::new(),
                });
            }
            Err(error) => return Err(error_from_file(&format!("read of {name}"), error)),
        };
        let mut complete_size = bytes.len() as u64;
        if complete_size > 0 && bytes[complete_size as usize - 1] != b'\n' {
            complete_size = match bytes.iter().rposition(|byte| *byte == b'\n') {
                Some(position) => position as u64 + 1,
                None => 0,
            };
            self.fs
                .truncate_file(path, complete_size, context)
                .await
                .map_err(|error| {
                    error_from_file(&format!("torn-line truncation of {name}"), error)
                })?;
        }
        let mut lines = Vec::new();
        let mut start = 0usize;
        let mut line_number = 1u64;
        for end in 0..complete_size as usize {
            if bytes[end] != b'\n' {
                continue;
            }
            let text = std::str::from_utf8(&bytes[start..end]).map_err(|error| {
                corruption(format!(
                    "Invalid UTF-8 in complete {name} line {line_number}: {error}"
                ))
            })?;
            lines.push(ParsedLine {
                value: parse(text, line_number)?,
                start: start as u64,
            });
            start = end + 1;
            line_number += 1;
        }
        Ok(ParsedFile {
            path: path.to_string(),
            lines,
        })
    }

    async fn resolve_file(&self, file: &str, context: &Context) -> StorageResult<String> {
        self.fs
            .join_path(&[self.directory.as_str(), file], context)
            .await
            .map_err(|error| error_from_file("path join", error))
    }
}

#[async_trait]
impl Storage for JsonlStorage {
    async fn commit(&self, writes: &[StorageWrite], context: &Context) -> StorageResult<Seq> {
        self.assert_usable()?;
        let mut prepared: PreparedMemoryCommit =
            self.memory.prepare_commit(writes.to_vec(), None)?;
        let encoded = self.encode_commit(prepared.seq(), prepared.writes())?;
        let reclamations = self.plan_reclamations(prepared.writes(), &encoded)?;
        let mut sidecars: Vec<(String, String)> = Vec::new();
        for (file, content) in &encoded.sidecars {
            let path = self.resolve_file(file, context).await?;
            sidecars.push((file.clone(), path.clone()));
            let result = self.fs.append_file(&path, content, context).await;
            if let Err(error) = result {
                return Err(self.poison(error_from_file(&format!("append to {file}"), error)));
            }
        }
        if self.fsync {
            for (file, path) in &sidecars {
                let result = self.fs.flush_file(path, context).await;
                if let Err(error) = result {
                    return Err(self.poison(error_from_file(&format!("flush of {file}"), error)));
                }
            }
        }
        let marker = self
            .fs
            .append_file(&self.main_path, &encoded.marker, context)
            .await;
        if let Err(error) = marker {
            return Err(self.poison(error_from_file(&format!("append to {MAIN_FILE}"), error)));
        }
        let seq = self.memory.apply_prepared(&mut prepared)?;
        self.adopt_sidecar_state(prepared.writes())?;
        self.reclaim_sidecars(&reclamations, context).await;
        Ok(seq)
    }

    async fn mint_id(&self) -> StorageResult<u64> {
        self.assert_usable()?;
        self.memory.mint_id().await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> StorageResult<Option<ConversationRecord>> {
        self.assert_usable()?;
        self.memory.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<ConversationRecord>> {
        self.assert_usable()?;
        self.memory
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &Context,
    ) -> StorageResult<Option<crate::types::StoredEntry>> {
        self.assert_usable()?;
        self.memory.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &Context,
    ) -> StorageResult<Option<crate::types::StoredEntry>> {
        self.assert_usable()?;
        self.memory
            .entry_in_conversation(conversation_id, id, context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &Context,
    ) -> StorageResult<Option<EntryRecord>> {
        self.assert_usable()?;
        self.memory
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<EntryRecord>> {
        self.assert_usable()?;
        self.memory
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(&self, id: TaskId, context: &Context) -> StorageResult<Option<TaskRecord>> {
        self.assert_usable()?;
        self.memory.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<TaskRecord>> {
        self.assert_usable()?;
        self.memory.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.assert_usable()?;
        self.memory.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<SubmissionRecord>> {
        self.assert_usable()?;
        self.memory
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &Context,
    ) -> StorageResult<Option<SubmissionRecord>> {
        self.assert_usable()?;
        self.memory
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<DocumentRecord>> {
        self.assert_usable()?;
        self.memory.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &Context,
    ) -> StorageResult<Option<StoredDocument>> {
        self.assert_usable()?;
        self.memory.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> StorageResult<Page<DocumentRecord>> {
        self.assert_usable()?;
        self.memory
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &Context) -> StorageResult<()> {
        self.closed.store(true, Ordering::SeqCst);
        self.memory.close(context).await
    }
}

/// Convenience alias used by storage module re-exports.
pub type MemoryFileSystem = crate::fs::StdFileSystem;
