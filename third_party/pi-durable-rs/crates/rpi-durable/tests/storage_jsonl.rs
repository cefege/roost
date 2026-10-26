//! JSONL backend tests: shared conformance suite (fresh + across reopen),
//! publication/reclamation anchors, torn-tail recovery, fsync ordering and
//! corruption detection.

mod test_support;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context};
use rpi_durable::errors::{FileError, StorageError, StorageResult};
use rpi_durable::fs::{DirEntry, FileSystem, StdFileSystem};
use rpi_durable::ids::{
    ConversationId, DocumentId, EntryId, ROOT_CONVERSATION_ID, TaskId, id_from_number,
};
use rpi_durable::storage::{JsonlStorage, JsonlStorageOptions};
use rpi_durable::testing::run_storage_conformance;
use rpi_durable::types::{
    ConversationRecord, DocumentContent, DocumentCreate, DocumentScope, EntryRecord, Storage,
    StorageWrite, TaskRecord, TaskState,
};
use test_support::{ReopeningStorage, TempDir, cleanup_temp_dirs};

type OpenFuture<'a> = Pin<Box<dyn Future<Output = StorageResult<Box<dyn Storage>>> + Send + 'a>>;

fn open_jsonl(dir: &TempDir, fsync: bool) -> OpenFuture<'_> {
    let directory = dir.path_str();
    Box::pin(async move {
        let fs: Arc<dyn FileSystem> = Arc::new(StdFileSystem::new(dir.path()));
        let storage = JsonlStorage::open(
            &directory,
            fs,
            JsonlStorageOptions { fsync },
            &BACKGROUND_CONTEXT,
        )
        .await?;
        Ok(Box::new(storage) as Box<dyn Storage>)
    })
}

#[tokio::test]
async fn jsonl_storage_conformance() {
    let result = run_storage_conformance(|| async {
        let dir = TempDir::new("rpi-durable-jsonl-conf");
        let storage = open_jsonl(&dir, false).await?;
        Ok(Arc::from(storage) as Arc<dyn Storage>)
    })
    .await;
    cleanup_temp_dirs("rpi-durable-jsonl-conf");
    assert!(
        result.is_ok(),
        "conformance failures:\n{}",
        result.err().unwrap_or_default()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jsonl_storage_conformance_across_reopen() {
    let result = run_storage_conformance(|| async {
        let dir = TempDir::new("rpi-durable-jsonl-conf-reopen");
        let storage = open_jsonl(&dir, false).await?;
        let reopen_dir = dir.path_str();
        let reopen_path = dir.path().to_path_buf();
        let reopened = ReopeningStorage::new(storage, move || {
            let directory = reopen_dir.clone();
            let path = reopen_path.clone();
            Box::pin(async move {
                let fs: Arc<dyn FileSystem> = Arc::new(StdFileSystem::new(path));
                let storage = JsonlStorage::open(
                    &directory,
                    fs,
                    JsonlStorageOptions::default(),
                    &BACKGROUND_CONTEXT,
                )
                .await?;
                Ok(Box::new(storage) as Box<dyn Storage>)
            })
        });
        Ok(Arc::new(reopened) as Arc<dyn Storage>)
    })
    .await;
    cleanup_temp_dirs("rpi-durable-jsonl-conf-reopen");
    assert!(
        result.is_ok(),
        "conformance failures:\n{}",
        result.err().unwrap_or_default()
    );
}

fn session_document(id: DocumentId, kind: &str) -> DocumentCreate {
    DocumentCreate {
        id,
        kind: kind.to_string(),
        key: None,
        scope: DocumentScope::Session,
        history: None,
        fork: None,
    }
}

fn task_record(id: TaskId, phase: &str, terminal: bool) -> TaskRecord {
    TaskRecord {
        id,
        conversation_id: ROOT_CONVERSATION_ID,
        kind: "test.task".to_string(),
        version: 1,
        input: serde_json::Value::Null,
        owner: None,
        background: false,
        abort_requested: false,
        started_at: None,
        ended_at: None,
        state: if terminal {
            TaskState::Terminal {
                outcome: rpi_durable::types::TaskOutcome::Completed {
                    result: serde_json::Value::Null,
                },
            }
        } else {
            TaskState::Pending {
                checkpoint: serde_json::json!({ "phase": phase }),
            }
        },
        memos: None,
    }
}

fn read_lines(path: &std::path::Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

async fn open_storage(dir: &TempDir, fsync: bool) -> JsonlStorage {
    let fs: Arc<dyn FileSystem> = Arc::new(StdFileSystem::new(dir.path()));
    JsonlStorage::open(
        &dir.path_str(),
        fs,
        JsonlStorageOptions { fsync },
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("open storage")
}

#[tokio::test]
async fn publishes_every_commit_before_reclaiming_sidecars() {
    let dir = TempDir::new("rpi-durable-jsonl-reclaim");
    let storage = open_storage(&dir, false).await;
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("root");
    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint"));
    let document_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint"));
    storage
        .commit(
            &[StorageWrite::Task {
                value: task_record(task_id, "ready", false),
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("task");
    storage
        .commit(
            &[StorageWrite::DocumentCreate {
                record: session_document(document_id, "test.document"),
                content: DocumentContent::Base {
                    version: 1,
                    value: serde_json::json!({ "count": 0 })
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("document");
    storage
        .commit(
            &[StorageWrite::DocumentChange {
                id: document_id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: Vec::new(),
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("delta");
    storage
        .commit(
            &[StorageWrite::DocumentChange {
                id: document_id,
                content: DocumentContent::Base {
                    version: 1,
                    value: serde_json::json!({ "count": 1 })
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("checkpoint");
    assert_eq!(
        read_lines(&dir.path().join(format!("task-{task_id}.jsonl"))).len(),
        1
    );
    assert_eq!(
        read_lines(&dir.path().join(format!("doc-{document_id}.jsonl"))).len(),
        1
    );

    storage
        .commit(
            &[StorageWrite::DocumentRetire { id: document_id }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("retire");
    storage
        .commit(
            &[StorageWrite::Task {
                value: task_record(task_id, "ready", true),
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("terminal task");

    assert_eq!(read_lines(&dir.path().join("main.jsonl")).len(), 7);
    assert!(!dir.path().join(format!("task-{task_id}.jsonl")).exists());
    assert!(!dir.path().join(format!("doc-{document_id}.jsonl")).exists());
    storage.close(&BACKGROUND_CONTEXT).await.expect("close");
    cleanup_temp_dirs("rpi-durable-jsonl-reclaim");
}

#[tokio::test]
async fn removes_torn_unconfirmed_tail_on_reopen() {
    let dir = TempDir::new("rpi-durable-jsonl-torn");
    let storage = open_storage(&dir, false).await;
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("root");
    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint"));
    storage
        .commit(
            &[StorageWrite::Task {
                value: task_record(task_id, "first", false),
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("task");
    storage.close(&BACKGROUND_CONTEXT).await.expect("close");
    // Append a complete-but-unconfirmed sidecar record and a torn tail.
    let sidecar = dir.path().join(format!("task-{task_id}.jsonl"));
    let mut content = std::fs::read_to_string(&sidecar).expect("read sidecar");
    let extra = serde_json::json!({
        "format": 1,
        "type": "record",
        "seq": 99,
        "ordinal": 0,
        "payload": {
            "type": "task",
            "value": task_record(task_id, "second", false)
        }
    });
    content.push_str(&serde_json::to_string(&extra).expect("extra record"));
    content.push('\n');
    content.push_str("{\"format\":1,\"type\":\"rec");
    std::fs::write(&sidecar, content).expect("write sidecar");

    let reopened = open_storage(&dir, false).await;
    assert!(
        reopened
            .task(task_id, &BACKGROUND_CONTEXT)
            .await
            .expect("task")
            .is_some()
    );
    let lines = read_lines(&sidecar);
    assert_eq!(
        lines.len(),
        1,
        "unconfirmed and torn records must be removed"
    );
    reopened.close(&BACKGROUND_CONTEXT).await.expect("close");
    cleanup_temp_dirs("rpi-durable-jsonl-torn");
}

#[tokio::test]
async fn corrupt_main_marker_rejects_open() {
    let dir = TempDir::new("rpi-durable-jsonl-corrupt");
    let storage = open_storage(&dir, false).await;
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("root");
    storage.close(&BACKGROUND_CONTEXT).await.expect("close");
    let main = dir.path().join("main.jsonl");
    let mut content = std::fs::read_to_string(&main).expect("read main");
    content.push_str("{\"format\":1,\"type\":\"commit\",\"seq\":1,\"writes\":[]}\n");
    std::fs::write(&main, content).expect("write main");

    let fs: Arc<dyn FileSystem> = Arc::new(StdFileSystem::new(dir.path()));
    let result = JsonlStorage::open(
        &dir.path_str(),
        fs,
        JsonlStorageOptions::default(),
        &BACKGROUND_CONTEXT,
    )
    .await;
    match result {
        Err(StorageError::JsonlCorruption(message)) => {
            assert!(
                message.contains("Commit sequence"),
                "unexpected message: {message}"
            );
        }
        Err(other) => panic!("expected corruption error, got {other}"),
        Ok(_) => panic!("expected corruption error, got success"),
    }
    cleanup_temp_dirs("rpi-durable-jsonl-corrupt");
}

/// Records fs operations in call order to assert sidecar-before-marker flush
/// ordering.
struct RecordingFileSystem {
    inner: StdFileSystem,
    operations: Arc<Mutex<Vec<String>>>,
}

impl RecordingFileSystem {
    fn new(cwd: std::path::PathBuf, operations: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            inner: StdFileSystem::new(cwd),
            operations,
        }
    }

    fn record(&self, operation: &str, path: &str) {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        if let Ok(mut operations) = self.operations.lock() {
            operations.push(format!("{operation}:{name}"));
        }
    }
}

#[async_trait::async_trait]
impl FileSystem for RecordingFileSystem {
    async fn absolute_path(&self, path: &str, context: &Context) -> Result<String, FileError> {
        self.inner.absolute_path(path, context).await
    }
    async fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        context: &Context,
    ) -> Result<(), FileError> {
        self.inner.create_dir(path, recursive, context).await
    }
    async fn join_path(&self, parts: &[&str], context: &Context) -> Result<String, FileError> {
        self.inner.join_path(parts, context).await
    }
    async fn append_file(
        &self,
        path: &str,
        content: &str,
        context: &Context,
    ) -> Result<(), FileError> {
        self.record("append", path);
        self.inner.append_file(path, content, context).await
    }
    async fn flush_file(&self, path: &str, context: &Context) -> Result<(), FileError> {
        self.record("flush", path);
        self.inner.flush_file(path, context).await
    }
    async fn remove(&self, path: &str, force: bool, context: &Context) -> Result<(), FileError> {
        self.record("remove", path);
        self.inner.remove(path, force, context).await
    }
    async fn write_file(
        &self,
        path: &str,
        content: &str,
        context: &Context,
    ) -> Result<(), FileError> {
        self.record("write", path);
        self.inner.write_file(path, content, context).await
    }
    async fn rename_file(&self, from: &str, to: &str, context: &Context) -> Result<(), FileError> {
        self.record("rename", from);
        self.inner.rename_file(from, to, context).await
    }
    async fn list_dir(&self, path: &str, context: &Context) -> Result<Vec<DirEntry>, FileError> {
        self.inner.list_dir(path, context).await
    }
    async fn read_binary_file(&self, path: &str, context: &Context) -> Result<Vec<u8>, FileError> {
        self.inner.read_binary_file(path, context).await
    }
    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &Context,
    ) -> Result<(), FileError> {
        self.record("truncate", path);
        self.inner.truncate_file(path, size, context).await
    }
}

async fn fsync_order(dir: &TempDir, fsync: bool) -> Vec<String> {
    let operations = Arc::new(Mutex::new(Vec::new()));
    let fs: Arc<dyn FileSystem> = Arc::new(RecordingFileSystem::new(
        dir.path().to_path_buf(),
        Arc::clone(&operations),
    ));
    let storage = JsonlStorage::open(
        &dir.path_str(),
        fs,
        JsonlStorageOptions { fsync },
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("open");
    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint"));
    storage
        .commit(
            &[StorageWrite::Task {
                value: task_record(task_id, "ready", false),
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("task");
    storage.close(&BACKGROUND_CONTEXT).await.expect("close");
    operations.lock().expect("operations").clone()
}

#[tokio::test]
async fn fsync_flushes_sidecars_before_the_marker() {
    let dir = TempDir::new("rpi-durable-jsonl-fsync-on");
    let operations = fsync_order(&dir, true).await;
    let flush_index = operations
        .iter()
        .position(|entry| entry.starts_with("flush:task-"))
        .expect("sidecar flush");
    let marker_index = operations
        .iter()
        .position(|entry| entry.starts_with("append:main.jsonl"))
        .expect("marker append");
    assert!(flush_index < marker_index, "operations: {operations:?}");
    cleanup_temp_dirs("rpi-durable-jsonl-fsync-on");
}

#[tokio::test]
async fn default_off_fsync_skips_sidecar_flushes() {
    let dir = TempDir::new("rpi-durable-jsonl-fsync-off");
    let operations = fsync_order(&dir, false).await;
    assert!(
        !operations.iter().any(|entry| entry.starts_with("flush:")),
        "operations: {operations:?}"
    );
    cleanup_temp_dirs("rpi-durable-jsonl-fsync-off");
}

#[tokio::test]
async fn indexes_entries_and_reports_entry_commit_sequence() {
    let dir = TempDir::new("rpi-durable-jsonl-entry");
    let storage = open_storage(&dir, false).await;
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("root");
    let entry_id: EntryId = id_from_number(storage.mint_id().await.expect("mint"));
    let seq = storage
        .commit(
            &[StorageWrite::Entry {
                value: EntryRecord::new(entry_id, ROOT_CONVERSATION_ID, "message"),
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("entry");
    let stored = storage
        .entry(entry_id, &BACKGROUND_CONTEXT)
        .await
        .expect("read")
        .expect("present");
    assert_eq!(stored.commit_seq, seq);
    let conversation: ConversationId = ROOT_CONVERSATION_ID;
    assert!(
        storage
            .conversation(conversation, &BACKGROUND_CONTEXT)
            .await
            .expect("conversation")
            .is_some()
    );
    storage.close(&BACKGROUND_CONTEXT).await.expect("close");
    cleanup_temp_dirs("rpi-durable-jsonl-entry");
}
