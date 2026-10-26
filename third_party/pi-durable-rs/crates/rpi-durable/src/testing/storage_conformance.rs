//! Shared storage conformance suite, ported from
//! `src/testing/storage-conformance.ts` @ `abe508e1b` (runner-independent
//! cases).
//!
//! The upstream `describe`/`expect`/`it` runner injection maps to a Rust case
//! list plus `async fn` cases returning `Result<(), String>`; backends run the
//! same list through [`run_storage_conformance`] (V17-13 §7.3 form difference).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::chord::context::BACKGROUND_CONTEXT;
use crate::chord::delta::Op;
use crate::chord::json::{JsonObject, JsonValue};
use crate::errors::{StorageError, StorageResult};
use crate::ids::{ConversationId, DocumentId, EntryId, SubmissionId, TaskId, id_from_number};
use crate::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent,
    DocumentCreate, DocumentPoint, DocumentQuery, DocumentScope, EntryQuery, EntryRecord,
    InputSubmissionStatus, ScanOrder, Storage, StorageWrite, SubmissionQuery, SubmissionRecord,
    TaskQuery, TaskRecord, TaskState,
};

/// Boxed case future.
pub type ConformanceFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// One runner-independent conformance case.
pub struct ConformanceCase {
    /// Case name (same text as the upstream suite).
    pub name: &'static str,
    /// Case body.
    pub run: fn(Arc<dyn Storage>) -> ConformanceFuture,
}

macro_rules! check {
    ($condition:expr, $($message:tt)*) => {
        if !($condition) {
            return Err(format!($($message)*));
        }
    };
}

macro_rules! check_eq {
    ($left:expr, $right:expr) => {{
        let left = &$left;
        let right = &$right;
        if left != right {
            return Err(format!("expected {:?} == {:?}", left, right));
        }
    }};
}

fn check_err<T>(result: StorageResult<T>, contains: &str) -> Result<(), String> {
    match result {
        Err(error) if error.to_string().contains(contains) => Ok(()),
        Err(error) => Err(format!(
            "expected error containing {contains:?}, got {error}"
        )),
        Ok(_) => Err(format!(
            "expected error containing {contains:?}, got success"
        )),
    }
}

fn check_rejected<T>(result: StorageResult<T>) -> Result<(), String> {
    match result {
        Err(StorageError::Rejected(_)) => Ok(()),
        Err(error) => Err(format!("expected StorageRejected, got {error}")),
        Ok(_) => Err("expected StorageRejected, got success".to_string()),
    }
}

async fn create_root(storage: &Arc<dyn Storage>) -> Result<ConversationId, String> {
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: crate::ids::ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            &BACKGROUND_CONTEXT,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(crate::ids::ROOT_CONVERSATION_ID)
}

fn pending_task(id: TaskId, conversation_id: ConversationId, phase: &str) -> TaskRecord {
    TaskRecord {
        id,
        conversation_id,
        kind: "test.task".to_string(),
        version: 1,
        input: serde_json::json!({ "value": id.get() }),
        owner: None,
        background: false,
        abort_requested: false,
        started_at: None,
        ended_at: None,
        state: TaskState::Pending {
            checkpoint: serde_json::json!({ "phase": phase }),
        },
        memos: None,
    }
}

fn entry(id: EntryId, conversation_id: ConversationId, kind: &str) -> EntryRecord {
    EntryRecord::new(id, conversation_id, kind)
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

fn input_queued(
    id: SubmissionId,
    conversation_id: ConversationId,
    request_id: Option<&str>,
) -> SubmissionRecord {
    SubmissionRecord::Input {
        id,
        conversation_id,
        request_id: request_id.map(str::to_string),
        status: InputSubmissionStatus::Queued,
    }
}

fn task_by_id(
    storage: &Arc<dyn Storage>,
    id: TaskId,
) -> impl Future<Output = Result<TaskRecord, String>> + '_ {
    let storage = Arc::clone(storage);
    async move {
        storage
            .task(id, &BACKGROUND_CONTEXT)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "task missing".to_string())
    }
}

fn submission_by_id(
    storage: &Arc<dyn Storage>,
    id: SubmissionId,
) -> impl Future<Output = Result<SubmissionRecord, String>> + '_ {
    let storage = Arc::clone(storage);
    async move {
        storage
            .submission(id, &BACKGROUND_CONTEXT)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "submission missing".to_string())
    }
}

fn case_1_reserves_root(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        check_eq!(storage.mint_id().await.map_err(|e| e.to_string())?, 2);
        let root = create_root(&storage).await?;
        check_eq!(root.get(), 1);
        let record = storage
            .conversation(root, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("root missing")?;
        check_eq!(
            record,
            ConversationRecord {
                id: root,
                parent: None,
                owner: None
            }
        );
        check_err(
            storage
                .commit(
                    &[StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: root,
                            parent: None,
                            owner: None,
                        },
                    }],
                    &BACKGROUND_CONTEXT,
                )
                .await,
            &format!("ID {} already belongs to conversation", root.get()),
        )
    })
}

fn case_2_mixed_commit_rollback(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let entry_id: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task_id: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let submission_id: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task = pending_task(task_id, root, "ready");
        let mut stored_entry = entry(entry_id, root, "user");
        stored_entry.data = Some(serde_json::json!({ "text": "hello" }));
        let input = SubmissionRecord::Input {
            id: submission_id,
            conversation_id: root,
            request_id: Some("request-1".to_string()),
            status: InputSubmissionStatus::Placed { entry: entry_id },
        };
        let initial_seq = storage
            .commit(
                &[
                    StorageWrite::Entry {
                        value: stored_entry.clone(),
                    },
                    StorageWrite::Task {
                        value: task.clone(),
                    },
                    StorageWrite::Submission {
                        value: input.clone(),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let stored = storage
            .entry(entry_id, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            stored.as_ref().map(|s| s.entry.clone()),
            Some(stored_entry.clone())
        );
        check_eq!(stored.as_ref().map(|s| s.commit_seq), Some(initial_seq));
        check_eq!(task_by_id(&storage, task_id).await?, task);
        check_eq!(submission_by_id(&storage, submission_id).await?, input);

        let transient_entry_id: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let mut running = task.clone();
        running.state = TaskState::Running {
            checkpoint: serde_json::json!({ "phase": "effect" }),
        };
        let done_input = SubmissionRecord::Input {
            id: submission_id,
            conversation_id: root,
            request_id: Some("request-1".to_string()),
            status: InputSubmissionStatus::Done {
                entry: entry_id,
                answer: transient_entry_id,
            },
        };
        check_err(
            storage
                .commit(
                    &[
                        StorageWrite::Task { value: running },
                        StorageWrite::Submission { value: done_input },
                        StorageWrite::Entry {
                            value: entry(transient_entry_id, root, "assistant"),
                        },
                        StorageWrite::Conversation {
                            value: ConversationRecord {
                                id: root,
                                parent: None,
                                owner: None,
                            },
                        },
                    ],
                    &BACKGROUND_CONTEXT,
                )
                .await,
            &format!("ID {} already belongs to conversation", root.get()),
        )?;
        check_eq!(task_by_id(&storage, task_id).await?, task);
        check_eq!(submission_by_id(&storage, submission_id).await?, input);
        check_eq!(
            storage
                .entry(transient_entry_id, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            true
        );
        let after = storage
            .commit(
                &[StorageWrite::Entry {
                    value: entry(
                        id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?),
                        root,
                        "after-rollback",
                    ),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check!(
            after > initial_seq,
            "rollback sequence {after:?} not greater than {initial_seq:?}"
        );
        Ok(())
    })
}

fn case_3_detaches_writes(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let entry_id: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task_id: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let submission_id: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let mut stored_entry = entry(entry_id, root, "note");
        stored_entry.data = Some(serde_json::json!({ "nested": [1, 2] }));
        let mut stored_task = pending_task(task_id, root, "ready");
        stored_task.state = TaskState::Pending {
            checkpoint: serde_json::json!({ "phase": "ready", "nested": { "count": 1 } }),
        };
        let stored_input = SubmissionRecord::Input {
            id: submission_id,
            conversation_id: root,
            request_id: None,
            status: InputSubmissionStatus::Unanswered {
                entry: None,
                reason: "failed".to_string(),
                detail: Some(serde_json::json!({ "codes": ["initial"] })),
            },
        };
        storage
            .commit(
                &[
                    StorageWrite::Entry {
                        value: stored_entry.clone(),
                    },
                    StorageWrite::Task {
                        value: stored_task.clone(),
                    },
                    StorageWrite::Submission {
                        value: stored_input.clone(),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        // Mutating the committed caller values must not affect stored state
        // (the Rust ownership boundary detaches writes by construction).
        let read_entry = storage
            .entry(entry_id, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("entry missing")?;
        check_eq!(read_entry.entry.data, stored_entry.data);
        check_eq!(
            task_by_id(&storage, task_id).await?.state,
            stored_task.state
        );
        check_eq!(
            submission_by_id(&storage, submission_id).await?.status(),
            "unanswered"
        );
        Ok(())
    })
}

fn case_4_prototype_like_keys(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let entry_id: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let data: JsonValue = serde_json::from_str(
            r#"{"__proto__":{"polluted":false},"constructor":{"label":"stored"},"toString":"value"}"#,
        )
        .map_err(|e| e.to_string())?;
        let mut stored = entry(entry_id, root, "note");
        stored.data = Some(data);
        storage
            .commit(
                &[StorageWrite::Entry {
                    value: stored.clone(),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let read = storage
            .entry(entry_id, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("entry missing")?;
        let value = read.entry.data.ok_or("data missing")?;
        check_eq!(
            value.get("__proto__"),
            Some(&serde_json::json!({ "polluted": false }))
        );
        check_eq!(
            value.get("constructor"),
            Some(&serde_json::json!({ "label": "stored" }))
        );
        check_eq!(
            value.get("toString"),
            Some(&JsonValue::String("value".to_string()))
        );
        Ok(())
    })
}

fn case_5_out_of_order_entries(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        storage
            .commit(
                &[
                    StorageWrite::Entry {
                        value: entry(id_from_number(30), root, "message"),
                    },
                    StorageWrite::Entry {
                        value: entry(id_from_number(10), root, "message"),
                    },
                    StorageWrite::Entry {
                        value: {
                            let mut marker = entry(id_from_number(20), root, "marker");
                            marker.head = Some(id_from_number(10));
                            marker
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let page = storage
            .scan_entries(
                EntryQuery {
                    conversation_id: root,
                    min_entry_id: None,
                    max_entry_id: None,
                    order: None,
                },
                10,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let ids: Vec<u64> = page.items.iter().map(|item| item.id.get()).collect();
        check_eq!(ids, vec![30, 20, 10]);
        let marker = storage
            .find_latest_head_marker(root, None, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(marker.map(|marker| marker.id.get()), Some(20));
        Ok(())
    })
}

fn case_6_entry_cursor_after_newer_commit(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let oldest: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let middle: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let newest: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[
                    StorageWrite::Entry {
                        value: entry(oldest, root, "message"),
                    },
                    StorageWrite::Entry {
                        value: entry(middle, root, "message"),
                    },
                    StorageWrite::Entry {
                        value: entry(newest, root, "message"),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let first = storage
            .scan_entries(
                EntryQuery {
                    conversation_id: root,
                    min_entry_id: None,
                    max_entry_id: None,
                    order: None,
                },
                2,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            first
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![newest.get(), middle.get()]
        );
        let appended: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[StorageWrite::Entry {
                    value: entry(appended, root, "message"),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let second = storage
            .scan_entries(
                EntryQuery {
                    conversation_id: root,
                    min_entry_id: None,
                    max_entry_id: None,
                    order: None,
                },
                2,
                first.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            second
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![oldest.get()]
        );
        check!(second.next.is_none(), "expected exhausted cursor");
        Ok(())
    })
}

fn case_7_conversation_cursor(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let second: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let third: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: third,
                            parent: None,
                            owner: None,
                        },
                    },
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: second,
                            parent: None,
                            owner: None,
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let first = storage
            .scan_conversations(ConversationQuery::default(), 2, None, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            first
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![root.get(), second.get()]
        );
        let cursor = first.next.clone().ok_or("expected cursor")?;
        let round_tripped: Cursor =
            serde_json::from_str(&serde_json::to_string(&cursor).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let second_page = storage
            .scan_conversations(
                ConversationQuery::default(),
                2,
                Some(round_tripped),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            second_page
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![third.get()]
        );
        check!(second_page.next.is_none(), "expected exhausted cursor");
        Ok(())
    })
}

fn case_8_owner_edges(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let other_owner: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let first_task: TaskId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second_task: TaskId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let first: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let third: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let owner = |conversation_id, task_id| {
            Some(crate::types::ConversationOwner {
                conversation_id,
                task_id,
            })
        };
        storage
            .commit(
                &[
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: other_owner,
                            parent: None,
                            owner: None,
                        },
                    },
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: first,
                            parent: None,
                            owner: owner(root, first_task),
                        },
                    },
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: second,
                            parent: None,
                            owner: owner(root, second_task),
                        },
                    },
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: third,
                            parent: None,
                            owner: owner(other_owner, first_task),
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let first_page = storage
            .scan_conversations(
                ConversationQuery {
                    owner_conversation_id: Some(root),
                    owner_task_id: None,
                    order: None,
                },
                1,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            first_page
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![first.get()]
        );
        let second_page = storage
            .scan_conversations(
                ConversationQuery {
                    owner_conversation_id: Some(root),
                    owner_task_id: None,
                    order: None,
                },
                1,
                first_page.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            second_page
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![second.get()]
        );
        check!(second_page.next.is_none(), "expected exhausted cursor");
        let by_task = storage
            .scan_conversations(
                ConversationQuery {
                    owner_conversation_id: None,
                    owner_task_id: Some(first_task),
                    order: None,
                },
                10,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            by_task
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![first.get(), third.get()]
        );
        let both = storage
            .scan_conversations(
                ConversationQuery {
                    owner_conversation_id: Some(root),
                    owner_task_id: Some(first_task),
                    order: None,
                },
                10,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            both.items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![first.get()]
        );
        Ok(())
    })
}

fn case_9_deep_fork_scan(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let root_first: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let root_fork_point: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let root_excluded_same_commit: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let root_seq = storage
            .commit(
                &[
                    StorageWrite::Entry {
                        value: entry(root_first, root, "message"),
                    },
                    StorageWrite::Entry {
                        value: {
                            let mut marker = entry(root_fork_point, root, "marker");
                            marker.head = Some(root_first);
                            marker
                        },
                    },
                    StorageWrite::Entry {
                        value: entry(root_excluded_same_commit, root, "message"),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let child: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: child,
                        parent: Some(crate::types::ConversationParent {
                            conversation_id: root,
                            at: root_fork_point,
                        }),
                        owner: None,
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let child_fork_point: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let child_excluded: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[
                    StorageWrite::Entry {
                        value: entry(child_fork_point, child, "note"),
                    },
                    StorageWrite::Entry {
                        value: entry(child_excluded, child, "message"),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let root_excluded_later: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[StorageWrite::Entry {
                    value: entry(root_excluded_later, root, "message"),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let grandchild: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: grandchild,
                        parent: Some(crate::types::ConversationParent {
                            conversation_id: child,
                            at: child_fork_point,
                        }),
                        owner: None,
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let grandchild_head: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let grandchild_tail: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let grandchild_seq = storage
            .commit(
                &[
                    StorageWrite::Entry {
                        value: {
                            let mut marker = entry(grandchild_head, grandchild, "marker");
                            marker.head = Some(grandchild_head);
                            marker
                        },
                    },
                    StorageWrite::Entry {
                        value: entry(grandchild_tail, grandchild, "message"),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let child_excluded_later: EntryId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[StorageWrite::Entry {
                    value: entry(child_excluded_later, child, "message"),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;

        let base = EntryQuery {
            conversation_id: grandchild,
            min_entry_id: None,
            max_entry_id: None,
            order: None,
        };
        let first = storage
            .scan_entries(base.clone(), 2, None, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            first
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![grandchild_tail.get(), grandchild_head.get()]
        );
        let second = storage
            .scan_entries(base.clone(), 2, first.next.clone(), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            second
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![child_fork_point.get(), root_fork_point.get()]
        );
        let third = storage
            .scan_entries(base.clone(), 2, second.next.clone(), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            third
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![root_first.get()]
        );
        check!(third.next.is_none(), "expected exhausted cursor");

        let ascending = EntryQuery {
            conversation_id: grandchild,
            min_entry_id: None,
            max_entry_id: None,
            order: Some(ScanOrder::Ascending),
        };
        let first_up = storage
            .scan_entries(ascending.clone(), 2, None, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            first_up
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![root_first.get(), root_fork_point.get()]
        );
        let second_up = storage
            .scan_entries(
                ascending.clone(),
                2,
                first_up.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            second_up
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![child_fork_point.get(), grandchild_head.get()]
        );
        let third_up = storage
            .scan_entries(base.clone(), 2, second_up.next.clone(), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            third_up
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![grandchild_tail.get()]
        );
        check!(third_up.next.is_none(), "expected exhausted cursor");
        let bounded = storage
            .scan_entries(
                EntryQuery {
                    conversation_id: grandchild,
                    min_entry_id: Some(root_fork_point),
                    max_entry_id: Some(child_fork_point),
                    order: Some(ScanOrder::Ascending),
                },
                10,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            bounded
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![root_fork_point.get(), child_fork_point.get()]
        );
        check_err(
            storage
                .scan_entries(
                    EntryQuery {
                        conversation_id: grandchild,
                        min_entry_id: None,
                        max_entry_id: None,
                        order: Some(ScanOrder::Descending),
                    },
                    2,
                    first_up.next.clone(),
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "cursor",
        )?;

        let current_marker = storage
            .find_latest_head_marker(grandchild, None, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("marker missing")?;
        check_eq!(current_marker.id.get(), grandchild_head.get());
        check_eq!(
            current_marker.head.map(EntryId::get),
            Some(grandchild_head.get())
        );
        let historical_marker = storage
            .find_latest_head_marker(grandchild, Some(child_fork_point), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("historical marker missing")?;
        check_eq!(historical_marker.id.get(), root_fork_point.get());
        check_eq!(
            historical_marker.head.map(EntryId::get),
            Some(root_first.get())
        );
        check!(
            storage
                .find_latest_head_marker(grandchild, Some(root_first), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "expected no marker below root first"
        );

        let active_first = storage
            .scan_entries(
                EntryQuery {
                    conversation_id: grandchild,
                    min_entry_id: current_marker.head,
                    max_entry_id: None,
                    order: None,
                },
                1,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            active_first
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![grandchild_tail.get()]
        );
        check!(active_first.next.is_some(), "expected active cursor");
        let active_second = storage
            .scan_entries(
                EntryQuery {
                    conversation_id: grandchild,
                    min_entry_id: current_marker.head,
                    max_entry_id: None,
                    order: None,
                },
                1,
                active_first.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            active_second
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            vec![grandchild_head.get()]
        );
        check!(
            active_second.next.is_none(),
            "expected exhausted active cursor"
        );

        check_eq!(
            storage
                .entry(root_first, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.commit_seq),
            Some(root_seq)
        );
        check_eq!(
            storage
                .entry(root_fork_point, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.commit_seq),
            Some(root_seq)
        );
        check_eq!(
            storage
                .entry(grandchild_head, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.commit_seq),
            Some(grandchild_seq)
        );
        check_eq!(
            storage
                .entry(grandchild_tail, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.commit_seq),
            Some(grandchild_seq)
        );
        check!(
            storage
                .entry(id_from_number(999_999), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "expected missing entry"
        );
        check_eq!(
            storage
                .entry_in_conversation(grandchild, root_first, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.entry.conversation_id),
            Some(root)
        );
        check_eq!(
            storage
                .entry_in_conversation(grandchild, child_fork_point, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.entry.conversation_id),
            Some(child)
        );
        check!(
            storage
                .entry_in_conversation(grandchild, root_excluded_same_commit, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "same-commit entry above fork point must be invisible"
        );
        check!(
            storage
                .entry_in_conversation(grandchild, root_excluded_later, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "later root entry must be invisible"
        );
        check!(
            storage
                .entry_in_conversation(grandchild, child_excluded, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "child entry above grandchild fork must be invisible"
        );
        check!(
            storage
                .entry_in_conversation(grandchild, child_excluded_later, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "later child entry must be invisible"
        );
        check!(
            storage
                .entry_in_conversation(root, grandchild_head, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "root must not see grandchild entry"
        );
        check_err(
            storage
                .entry_in_conversation(id_from_number(999_999), root_first, &BACKGROUND_CONTEXT)
                .await,
            "Unknown conversation",
        )?;
        check_eq!(
            storage
                .scan_entries(
                    EntryQuery {
                        conversation_id: grandchild,
                        min_entry_id: historical_marker.head,
                        max_entry_id: Some(child_fork_point),
                        order: None,
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|entry| entry.id.get())
                .collect::<Vec<_>>(),
            vec![
                child_fork_point.get(),
                root_fork_point.get(),
                root_first.get()
            ]
        );
        check_eq!(
            storage
                .entry_in_conversation(grandchild, grandchild_tail, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| stored.commit_seq),
            Some(grandchild_seq)
        );
        check!(
            storage
                .entry_in_conversation(grandchild, id_from_number(999_999), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "missing entry must read as absent"
        );
        check_err(
            storage
                .scan_entries(
                    EntryQuery {
                        conversation_id: id_from_number(999_999),
                        min_entry_id: None,
                        max_entry_id: None,
                        order: None,
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "Unknown conversation",
        )?;
        Ok(())
    })
}

fn case_10_table_order_and_cursor(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let mut conversation_ids = vec![root];
        let mut task_ids: Vec<TaskId> = Vec::new();
        let mut submission_ids: Vec<SubmissionId> = Vec::new();
        for _ in 0..3 {
            let conversation: ConversationId =
                id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
            let task: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
            let submission: SubmissionId =
                id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
            storage
                .commit(
                    &[
                        StorageWrite::Conversation {
                            value: ConversationRecord {
                                id: conversation,
                                parent: None,
                                owner: None,
                            },
                        },
                        StorageWrite::Task {
                            value: pending_task(task, root, "ready"),
                        },
                        StorageWrite::Submission {
                            value: input_queued(submission, root, None),
                        },
                    ],
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?;
            conversation_ids.push(conversation);
            task_ids.push(task);
            submission_ids.push(submission);
        }
        let expected_conversations: Vec<u64> = conversation_ids.iter().map(|id| id.get()).collect();
        let expected_tasks: Vec<u64> = task_ids.iter().map(|id| id.get()).collect();
        let expected_submissions: Vec<u64> = submission_ids.iter().map(|id| id.get()).collect();
        let all_conversations =
            async |order: Option<ScanOrder>, cursor: Option<Cursor>| -> Result<Vec<u64>, String> {
                let mut found = Vec::new();
                let mut page = storage
                    .scan_conversations(
                        ConversationQuery {
                            order,
                            ..Default::default()
                        },
                        2,
                        cursor,
                        &BACKGROUND_CONTEXT,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                found.extend(page.items.iter().map(|item| item.id.get()));
                while let Some(next) = page.next.clone() {
                    page = storage
                        .scan_conversations(
                            ConversationQuery::default(),
                            2,
                            Some(next),
                            &BACKGROUND_CONTEXT,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    found.extend(page.items.iter().map(|item| item.id.get()));
                }
                Ok(found)
            };
        let mut reversed_conversations = expected_conversations.clone();
        reversed_conversations.reverse();
        check_eq!(all_conversations(None, None).await?, expected_conversations);
        check_eq!(
            all_conversations(Some(ScanOrder::Ascending), None).await?,
            expected_conversations
        );
        check_eq!(
            all_conversations(Some(ScanOrder::Descending), None).await?,
            reversed_conversations
        );
        let descending = storage
            .scan_conversations(
                ConversationQuery {
                    order: Some(ScanOrder::Descending),
                    ..Default::default()
                },
                2,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let continued = storage
            .scan_conversations(
                ConversationQuery::default(),
                2,
                descending.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            continued
                .items
                .iter()
                .map(|item| item.id.get())
                .collect::<Vec<_>>(),
            reversed_conversations[2..4].to_vec()
        );
        check_err(
            storage
                .scan_conversations(
                    ConversationQuery {
                        order: Some(ScanOrder::Ascending),
                        ..Default::default()
                    },
                    2,
                    descending.next,
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "cursor",
        )?;
        // Tasks and submissions follow the same order/cursor contract.
        let all_tasks =
            async |order: Option<ScanOrder>, cursor: Option<Cursor>| -> Result<Vec<u64>, String> {
                let mut found = Vec::new();
                let mut page = storage
                    .scan_tasks(
                        TaskQuery {
                            order,
                            ..Default::default()
                        },
                        2,
                        cursor,
                        &BACKGROUND_CONTEXT,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                found.extend(page.items.iter().map(|task| task.id.get()));
                while let Some(next) = page.next.clone() {
                    page = storage
                        .scan_tasks(TaskQuery::default(), 2, Some(next), &BACKGROUND_CONTEXT)
                        .await
                        .map_err(|e| e.to_string())?;
                    found.extend(page.items.iter().map(|task| task.id.get()));
                }
                Ok(found)
            };
        let all_submissions =
            async |order: Option<ScanOrder>, cursor: Option<Cursor>| -> Result<Vec<u64>, String> {
                let mut found = Vec::new();
                let mut page = storage
                    .scan_submissions(
                        SubmissionQuery {
                            order,
                            ..Default::default()
                        },
                        2,
                        cursor,
                        &BACKGROUND_CONTEXT,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                found.extend(page.items.iter().map(|submission| submission.id().get()));
                while let Some(next) = page.next.clone() {
                    page = storage
                        .scan_submissions(
                            SubmissionQuery::default(),
                            2,
                            Some(next),
                            &BACKGROUND_CONTEXT,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    found.extend(page.items.iter().map(|submission| submission.id().get()));
                }
                Ok(found)
            };
        let mut reversed_tasks = expected_tasks.clone();
        reversed_tasks.reverse();
        check_eq!(all_tasks(None, None).await?, expected_tasks);
        check_eq!(
            all_tasks(Some(ScanOrder::Ascending), None).await?,
            expected_tasks
        );
        check_eq!(
            all_tasks(Some(ScanOrder::Descending), None).await?,
            reversed_tasks
        );
        let mut reversed_submissions = expected_submissions.clone();
        reversed_submissions.reverse();
        check_eq!(all_submissions(None, None).await?, expected_submissions);
        check_eq!(
            all_submissions(Some(ScanOrder::Ascending), None).await?,
            expected_submissions
        );
        check_eq!(
            all_submissions(Some(ScanOrder::Descending), None).await?,
            reversed_submissions
        );
        let descending_tasks = storage
            .scan_tasks(
                TaskQuery {
                    order: Some(ScanOrder::Descending),
                    ..Default::default()
                },
                2,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let continued_tasks = storage
            .scan_tasks(
                TaskQuery::default(),
                2,
                descending_tasks.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            continued_tasks
                .items
                .iter()
                .map(|task| task.id.get())
                .collect::<Vec<_>>(),
            reversed_tasks
                .iter()
                .skip(2)
                .take(2)
                .copied()
                .collect::<Vec<_>>()
        );
        check_err(
            storage
                .scan_tasks(
                    TaskQuery {
                        order: Some(ScanOrder::Ascending),
                        ..Default::default()
                    },
                    2,
                    descending_tasks.next,
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "cursor",
        )?;
        let descending_submissions = storage
            .scan_submissions(
                SubmissionQuery {
                    order: Some(ScanOrder::Descending),
                    ..Default::default()
                },
                2,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let continued_submissions = storage
            .scan_submissions(
                SubmissionQuery::default(),
                2,
                descending_submissions.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            continued_submissions
                .items
                .iter()
                .map(|submission| submission.id().get())
                .collect::<Vec<_>>(),
            reversed_submissions
                .iter()
                .skip(2)
                .take(2)
                .copied()
                .collect::<Vec<_>>()
        );
        check_err(
            storage
                .scan_submissions(
                    SubmissionQuery {
                        order: Some(ScanOrder::Ascending),
                        ..Default::default()
                    },
                    2,
                    descending_submissions.next,
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "cursor",
        )?;
        Ok(())
    })
}

fn case_11_task_replacement_and_pages(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let first: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let third: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let mut first_task = pending_task(first, root, "ready");
        first_task.memos = Some({
            let mut memos = JsonObject::new();
            memos.insert("winner".to_string(), JsonValue::String("first".to_string()));
            memos
        });
        let mut second_task = pending_task(second, root, "ready");
        second_task.background = true;
        let mut third_task = pending_task(third, root, "ready");
        third_task.abort_requested = true;
        storage
            .commit(
                &[
                    StorageWrite::Task {
                        value: first_task.clone(),
                    },
                    StorageWrite::Task {
                        value: second_task.clone(),
                    },
                    StorageWrite::Task {
                        value: third_task.clone(),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;

        let mut running = first_task.clone();
        running.state = TaskState::Running {
            checkpoint: serde_json::json!({ "phase": "effect", "attempt": 1 }),
        };
        running.abort_requested = true;
        storage
            .commit(
                &[StorageWrite::Task {
                    value: running.clone(),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(task_by_id(&storage, first).await?, running);
        let terminal = TaskRecord {
            state: TaskState::Terminal {
                outcome: crate::types::TaskOutcome::Completed {
                    result: serde_json::json!({ "entryId": 99 }),
                },
            },
            ..running
        };
        storage
            .commit(
                &[StorageWrite::Task {
                    value: terminal.clone(),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(task_by_id(&storage, first).await?, terminal);

        let pending_page = storage
            .scan_tasks(
                TaskQuery {
                    status: Some(crate::types::TaskStatus::Pending),
                    ..Default::default()
                },
                1,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            pending_page
                .items
                .iter()
                .map(|task| task.id.get())
                .collect::<Vec<_>>(),
            vec![second.get()]
        );
        check!(pending_page.next.is_some(), "expected task cursor");
        let pending_second = storage
            .scan_tasks(
                TaskQuery {
                    status: Some(crate::types::TaskStatus::Pending),
                    ..Default::default()
                },
                1,
                pending_page.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            pending_second
                .items
                .iter()
                .map(|task| task.id.get())
                .collect::<Vec<_>>(),
            vec![third.get()]
        );
        let terminal_page = storage
            .scan_tasks(
                TaskQuery {
                    status: Some(crate::types::TaskStatus::Terminal),
                    abort_requested: Some(true),
                    ..Default::default()
                },
                10,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(terminal_page.items, vec![terminal]);
        let background_page = storage
            .scan_tasks(
                TaskQuery {
                    background: Some(true),
                    ..Default::default()
                },
                10,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            background_page
                .items
                .iter()
                .map(|task| task.id.get())
                .collect::<Vec<_>>(),
            vec![second.get()]
        );
        Ok(())
    })
}

fn case_12_task_owners_and_status(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let owner: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let waiting: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let sleeping: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let completing: TaskId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let owner_task = pending_task(owner, root, "ready");
        let mut waiting_task = pending_task(waiting, root, "next");
        waiting_task.owner = Some(owner);
        waiting_task.state = TaskState::Waiting {
            checkpoint: serde_json::json!({ "phase": "next" }),
            on: vec![owner],
            policy: crate::types::JoinPolicy::AllSettled,
        };
        let mut sleeping_task = pending_task(sleeping, root, "next");
        sleeping_task.owner = Some(owner);
        sleeping_task.state = TaskState::Sleeping {
            checkpoint: serde_json::json!({ "phase": "next" }),
            until: 61_000.0,
        };
        waiting_task.memos = Some({
            let mut memos = JsonObject::new();
            memos.insert("kept".to_string(), JsonValue::Bool(true));
            memos
        });
        let mut completing_task = pending_task(completing, root, "ready");
        completing_task.owner = Some(owner);
        completing_task.state = TaskState::Completing {
            outcome: crate::types::TaskOutcome::Failed {
                error: crate::types::TaskOutcomeError {
                    message: "held".to_string(),
                    detail: None,
                },
                result: None,
            },
        };
        storage
            .commit(
                &[
                    StorageWrite::Task {
                        value: owner_task.clone(),
                    },
                    StorageWrite::Task {
                        value: waiting_task.clone(),
                    },
                    StorageWrite::Task {
                        value: sleeping_task.clone(),
                    },
                    StorageWrite::Task {
                        value: completing_task.clone(),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(task_by_id(&storage, waiting).await?, waiting_task);
        check_eq!(task_by_id(&storage, completing).await?, completing_task);
        check_eq!(task_by_id(&storage, sleeping).await?, sleeping_task.clone());
        let scan = |status| {
            let storage = Arc::clone(&storage);
            async move {
                storage
                    .scan_tasks(
                        TaskQuery {
                            status: Some(status),
                            ..Default::default()
                        },
                        10,
                        None,
                        &BACKGROUND_CONTEXT,
                    )
                    .await
                    .map_err(|e| e.to_string())
                    .map(|page| page.items)
            }
        };
        check_eq!(
            scan(crate::types::TaskStatus::Waiting).await?,
            vec![waiting_task.clone()]
        );
        check_eq!(
            scan(crate::types::TaskStatus::Sleeping).await?,
            vec![sleeping_task]
        );
        check_eq!(
            scan(crate::types::TaskStatus::Completing).await?,
            vec![completing_task.clone()]
        );
        check_eq!(
            scan(crate::types::TaskStatus::Pending)
                .await?
                .iter()
                .map(|task| task.id.get())
                .collect::<Vec<_>>(),
            vec![owner.get()]
        );
        let outcome = match &completing_task.state {
            TaskState::Completing { outcome } => outcome.clone(),
            _ => return Err("expected completing state".to_string()),
        };
        let terminal = TaskRecord {
            state: TaskState::Terminal { outcome },
            ..completing_task.clone()
        };
        storage
            .commit(
                &[StorageWrite::Task {
                    value: terminal.clone(),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            scan(crate::types::TaskStatus::Completing).await?,
            Vec::<TaskRecord>::new()
        );
        check_eq!(
            scan(crate::types::TaskStatus::Terminal).await?,
            vec![terminal]
        );
        Ok(())
    })
}

fn case_13_request_ids(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let second_conversation: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: second_conversation,
                        parent: None,
                        owner: None,
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let first: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let other: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let first_record = input_queued(first, root, Some("same"));
        let second_record = input_queued(second, root, Some("other"));
        let other_record = input_queued(other, second_conversation, Some("same"));
        storage
            .commit(
                &[
                    StorageWrite::Submission {
                        value: first_record.clone(),
                    },
                    StorageWrite::Submission {
                        value: second_record.clone(),
                    },
                    StorageWrite::Submission {
                        value: other_record.clone(),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            storage
                .submission_by_request(root, "same", &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?,
            Some(first_record.clone())
        );
        check_eq!(
            storage
                .submission_by_request(second_conversation, "same", &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?,
            Some(other_record.clone())
        );
        let placed = SubmissionRecord::Input {
            id: second,
            conversation_id: root,
            request_id: Some("other".to_string()),
            status: InputSubmissionStatus::Placed {
                entry: id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?),
            },
        };
        storage
            .commit(
                &[StorageWrite::Submission {
                    value: placed.clone(),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(submission_by_id(&storage, second).await?, placed.clone());
        check_eq!(
            storage
                .submission_by_request(root, "other", &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?,
            Some(placed.clone())
        );
        let ids = |query: SubmissionQuery| {
            let storage = Arc::clone(&storage);
            async move {
                let mut found: Vec<SubmissionId> = Vec::new();
                let mut cursor: Option<Cursor> = None;
                loop {
                    let page = storage
                        .scan_submissions(query.clone(), 1, cursor.clone(), &BACKGROUND_CONTEXT)
                        .await
                        .map_err(|e| e.to_string())?;
                    found.extend(page.items.iter().map(|item| item.id()));
                    cursor = page.next.clone();
                    if cursor.is_none() {
                        break;
                    }
                }
                Ok::<Vec<SubmissionId>, String>(found)
            }
        };
        check_eq!(
            ids(SubmissionQuery::default()).await?,
            vec![first, second, other]
        );
        check_eq!(
            ids(SubmissionQuery {
                conversation_id: Some(root),
                ..Default::default()
            })
            .await?,
            vec![first, second]
        );
        check_eq!(
            ids(SubmissionQuery {
                status: Some(crate::types::SubmissionStatusFilter::Queued),
                ..Default::default()
            })
            .await?,
            vec![first, other]
        );
        check_eq!(
            ids(SubmissionQuery {
                status: Some(crate::types::SubmissionStatusFilter::Placed),
                ..Default::default()
            })
            .await?,
            vec![second]
        );
        check_eq!(
            ids(SubmissionQuery {
                conversation_id: Some(second_conversation),
                status: Some(crate::types::SubmissionStatusFilter::Placed),
                ..Default::default()
            })
            .await?,
            Vec::<SubmissionId>::new()
        );
        check_eq!(
            storage
                .scan_submissions(
                    SubmissionQuery {
                        status: Some(crate::types::SubmissionStatusFilter::Placed),
                        ..Default::default()
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items,
            vec![placed]
        );
        Ok(())
    })
}

fn case_14_passive_submissions(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let done_id: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let failed_id: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let queued_done = SubmissionRecord::Write {
            id: done_id,
            conversation_id: root,
            request_id: Some("passive-done".to_string()),
            status: crate::types::WriteSubmissionStatus::Queued,
        };
        let queued_failed = SubmissionRecord::Write {
            id: failed_id,
            conversation_id: root,
            request_id: Some("passive-failed".to_string()),
            status: crate::types::WriteSubmissionStatus::Queued,
        };
        storage
            .commit(
                &[
                    StorageWrite::Submission {
                        value: queued_done.clone(),
                    },
                    StorageWrite::Submission {
                        value: queued_failed.clone(),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let done = SubmissionRecord::Write {
            id: done_id,
            conversation_id: root,
            request_id: Some("passive-done".to_string()),
            status: crate::types::WriteSubmissionStatus::Done {
                entry: id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?),
            },
        };
        let unanswered = SubmissionRecord::Write {
            id: failed_id,
            conversation_id: root,
            request_id: Some("passive-failed".to_string()),
            status: crate::types::WriteSubmissionStatus::Unanswered {
                reason: "closed".to_string(),
                detail: Some(serde_json::json!({ "retryable": false })),
            },
        };
        storage
            .commit(
                &[
                    StorageWrite::Submission {
                        value: done.clone(),
                    },
                    StorageWrite::Submission {
                        value: unanswered.clone(),
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(submission_by_id(&storage, done_id).await?, done);
        check_eq!(
            storage
                .submission_by_request(root, "passive-done", &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?,
            Some(done)
        );
        check_eq!(submission_by_id(&storage, failed_id).await?, unanswered);
        Ok(())
    })
}

fn case_15_rewindable_documents(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let first: DocumentId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let first_record = DocumentCreate {
            id: first,
            kind: "conversation.notes".to_string(),
            key: None,
            scope: DocumentScope::Conversation {
                conversation_id: root,
            },
            history: Some(crate::types::DocumentHistory::Rewindable),
            fork: Some(crate::types::DocumentFork::AsOf),
        };
        let initial: JsonObject = {
            let mut value = JsonObject::new();
            value.insert("items".to_string(), serde_json::json!(["a"]));
            value.insert("nested".to_string(), serde_json::json!({ "count": 1 }));
            value
        };
        let created_at = storage
            .commit(
                &[StorageWrite::DocumentCreate {
                    record: first_record.clone(),
                    content: DocumentContent::Base {
                        version: 1,
                        value: initial.clone(),
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let ops = vec![
            Op::Patch {
                path: vec![crate::chord::delta::PathSegment::Key("items".to_string())],
                index: 1,
                remove: 0,
                items: vec![JsonValue::String("b".to_string())],
            },
            Op::Set {
                path: vec![
                    crate::chord::delta::PathSegment::Key("nested".to_string()),
                    crate::chord::delta::PathSegment::Key("count".to_string()),
                ],
                value: serde_json::json!(2),
            },
        ];
        let changed_at = storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id: first,
                    content: DocumentContent::Delta { version: 1, ops },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let created = storage
            .document(first, DocumentPoint::At(created_at), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("created document missing")?;
        check_eq!(created.version, 1);
        check_eq!(created.value, initial);
        check_eq!(created.deltas_since_base, 0);
        let changed = storage
            .document(first, DocumentPoint::At(changed_at), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("changed document missing")?;
        check_eq!(
            changed.value.get("items"),
            Some(&serde_json::json!(["a", "b"]))
        );
        check_eq!(
            changed.value.get("nested"),
            Some(&serde_json::json!({ "count": 2 }))
        );
        check_eq!(changed.deltas_since_base, 1);
        let current = storage
            .document(first, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("current document missing")?;
        check_eq!(
            current.value.get("items"),
            Some(&serde_json::json!(["a", "b"]))
        );

        let checkpoint_value: JsonObject = {
            let mut value = JsonObject::new();
            value.insert("items".to_string(), serde_json::json!(["checkpoint"]));
            value.insert("nested".to_string(), serde_json::json!({ "count": 3 }));
            value
        };
        let checkpoint_at = storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id: first,
                    content: DocumentContent::Base {
                        version: 2,
                        value: checkpoint_value.clone(),
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let replaced = storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id: first,
                    content: DocumentContent::Delta {
                        version: 2,
                        ops: vec![Op::Replace(serde_json::json!({ "items": ["replacement"], "nested": { "count": 4 } }))],
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            storage
                .document(first, DocumentPoint::At(changed_at), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| stored.value),
            Some({
                let mut value = JsonObject::new();
                value.insert("items".to_string(), serde_json::json!(["a", "b"]));
                value.insert("nested".to_string(), serde_json::json!({ "count": 2 }));
                value
            })
        );
        let checkpoint = storage
            .document(first, DocumentPoint::At(checkpoint_at), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("checkpoint missing")?;
        check_eq!(checkpoint.version, 2);
        check_eq!(checkpoint.value, checkpoint_value);
        check_eq!(checkpoint.deltas_since_base, 0);
        let replaced_doc = storage
            .document(first, DocumentPoint::At(replaced), &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("replaced missing")?;
        check_eq!(
            replaced_doc.value.get("items"),
            Some(&serde_json::json!(["replacement"]))
        );
        check_eq!(replaced_doc.deltas_since_base, 1);

        let second: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let retired_at = storage
            .commit(
                &[
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: second,
                            ..first_record.clone()
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "items": ["new"] })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentRetire { id: first },
                    StorageWrite::DocumentChange {
                        id: first,
                        content: DocumentContent::Delta {
                            version: 2,
                            ops: vec![Op::Set {
                                path: vec![crate::chord::delta::PathSegment::Key(
                                    "retiring".to_string(),
                                )],
                                value: JsonValue::Bool(true),
                            }],
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let address = DocumentAddress {
            kind: first_record.kind.clone(),
            scope: first_record.scope.clone(),
            key: None,
        };
        check_eq!(
            storage
                .find_document(
                    address.clone(),
                    DocumentPoint::At(changed_at),
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(first)
        );
        check_eq!(
            storage
                .find_document(
                    address.clone(),
                    DocumentPoint::At(retired_at),
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| (record.id, record.created_at)),
            Some((second, retired_at))
        );
        check_eq!(
            storage
                .scan_documents(
                    DocumentQuery {
                        scope: first_record.scope.clone(),
                        at: DocumentPoint::At(changed_at),
                        kind: None,
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|record| record.id.get())
                .collect::<Vec<_>>(),
            vec![first.get()]
        );
        check_eq!(
            storage
                .scan_documents(
                    DocumentQuery {
                        scope: first_record.scope.clone(),
                        at: DocumentPoint::At(retired_at),
                        kind: None,
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|record| record.id.get())
                .collect::<Vec<_>>(),
            vec![second.get()]
        );
        check!(
            storage
                .document(first, DocumentPoint::At(retired_at), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "retired incarnation must read as absent"
        );
        check_eq!(
            storage
                .document(second, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .and_then(|stored| stored.value.get("items").cloned()),
            Some(serde_json::json!(["new"]))
        );
        Ok(())
    })
}

fn case_16_long_tails(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let id: DocumentId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let record = DocumentCreate {
            id,
            kind: "conversation.long-tail".to_string(),
            key: None,
            scope: DocumentScope::Conversation {
                conversation_id: root,
            },
            history: Some(crate::types::DocumentHistory::Rewindable),
            fork: Some(crate::types::DocumentFork::AsOf),
        };
        let rows: Vec<JsonValue> = (0..512)
            .map(|value| serde_json::json!({ "value": value, "stable": format!("row-{value}") }))
            .collect();
        let mut initial = JsonObject::new();
        initial.insert("revision".to_string(), serde_json::json!(0));
        initial.insert("rows".to_string(), JsonValue::Array(rows));
        let created_at = storage
            .commit(
                &[StorageWrite::DocumentCreate {
                    record,
                    content: DocumentContent::Base {
                        version: 1,
                        value: initial.clone(),
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut before_replacement = initial.clone();
        let mut before_replacement_at = created_at;
        let key = crate::chord::delta::PathSegment::Key("rows".to_string());
        let value_key = crate::chord::delta::PathSegment::Key("value".to_string());
        for revision in 1..=24i64 {
            let index = ((revision * 17) % 512) as u64;
            if let Some(JsonValue::Array(rows)) = before_replacement.get_mut("rows")
                && let Some(JsonValue::Object(row)) = rows.get_mut(index as usize)
            {
                row.insert("value".to_string(), serde_json::json!(-revision));
            }
            before_replacement.insert("revision".to_string(), serde_json::json!(revision));
            before_replacement_at = storage
                .commit(
                    &[StorageWrite::DocumentChange {
                        id,
                        content: DocumentContent::Delta {
                            version: 1,
                            ops: vec![
                                Op::Set {
                                    path: vec![
                                        key.clone(),
                                        crate::chord::delta::PathSegment::Index(index),
                                        value_key.clone(),
                                    ],
                                    value: serde_json::json!(-revision),
                                },
                                Op::Set {
                                    path: vec![crate::chord::delta::PathSegment::Key(
                                        "revision".to_string(),
                                    )],
                                    value: serde_json::json!(revision),
                                },
                            ],
                        },
                    }],
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        let replacement_rows: Vec<JsonValue> = (0..512)
            .map(|value| serde_json::json!({ "value": 10_000 + value, "stable": format!("new-{value}") }))
            .collect();
        let mut replacement = JsonObject::new();
        replacement.insert("revision".to_string(), serde_json::json!(100));
        replacement.insert("rows".to_string(), JsonValue::Array(replacement_rows));
        let replacement_at = storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Replace(JsonValue::Object(replacement.clone()))],
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut current = replacement.clone();
        for revision in 101..=124i64 {
            let index = ((revision * 19) % 512) as u64;
            if let Some(JsonValue::Array(rows)) = current.get_mut("rows")
                && let Some(JsonValue::Object(row)) = rows.get_mut(index as usize)
            {
                row.insert("value".to_string(), serde_json::json!(-revision));
            }
            current.insert("revision".to_string(), serde_json::json!(revision));
            storage
                .commit(
                    &[StorageWrite::DocumentChange {
                        id,
                        content: DocumentContent::Delta {
                            version: 1,
                            ops: vec![
                                Op::Set {
                                    path: vec![
                                        key.clone(),
                                        crate::chord::delta::PathSegment::Index(index),
                                        value_key.clone(),
                                    ],
                                    value: serde_json::json!(-revision),
                                },
                                Op::Set {
                                    path: vec![crate::chord::delta::PathSegment::Key(
                                        "revision".to_string(),
                                    )],
                                    value: serde_json::json!(revision),
                                },
                            ],
                        },
                    }],
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        check_eq!(
            storage
                .document(id, DocumentPoint::At(created_at), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| stored.value),
            Some(initial)
        );
        check_eq!(
            storage
                .document(
                    id,
                    DocumentPoint::At(before_replacement_at),
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| stored.value),
            Some(before_replacement)
        );
        check_eq!(
            storage
                .document(id, DocumentPoint::At(replacement_at), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| stored.value),
            Some(replacement)
        );
        check_eq!(
            storage
                .document(id, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| stored.value),
            Some(current)
        );
        Ok(())
    })
}

fn case_17_document_copies(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let child: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second_child: ConversationId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: child,
                            parent: None,
                            owner: None,
                        },
                    },
                    StorageWrite::Conversation {
                        value: ConversationRecord {
                            id: second_child,
                            parent: None,
                            owner: None,
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let source: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let source_record = DocumentCreate {
            id: source,
            kind: "copy.source".to_string(),
            key: None,
            scope: DocumentScope::Conversation {
                conversation_id: root,
            },
            history: Some(crate::types::DocumentHistory::Rewindable),
            fork: Some(crate::types::DocumentFork::AsOf),
        };
        let created_at = storage
            .commit(
                &[StorageWrite::DocumentCreate {
                    record: source_record.clone(),
                    content: DocumentContent::Base {
                        version: 2,
                        value: serde_json::json!({ "count": 1, "rows": [{ "value": "base" }] })
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id: source,
                    content: DocumentContent::Delta {
                        version: 2,
                        ops: vec![
                            Op::Set {
                                path: vec![crate::chord::delta::PathSegment::Key(
                                    "count".to_string(),
                                )],
                                value: serde_json::json!(2),
                            },
                            Op::Patch {
                                path: vec![crate::chord::delta::PathSegment::Key(
                                    "rows".to_string(),
                                )],
                                index: 1,
                                remove: 0,
                                items: vec![serde_json::json!({ "value": "current" })],
                            },
                        ],
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let historical_copy: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let current_copy: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let retired_copy: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let child_record = |id: DocumentId, conversation_id: ConversationId| DocumentCreate {
            id,
            kind: source_record.kind.clone(),
            key: None,
            scope: DocumentScope::Conversation { conversation_id },
            history: Some(crate::types::DocumentHistory::Rewindable),
            fork: Some(crate::types::DocumentFork::AsOf),
        };
        storage
            .commit(
                &[
                    StorageWrite::DocumentCopy {
                        record: child_record(historical_copy, child),
                        source: crate::types::DocumentCopySource {
                            id: source,
                            at: DocumentPoint::At(created_at),
                        },
                    },
                    StorageWrite::DocumentCopy {
                        record: child_record(current_copy, second_child),
                        source: crate::types::DocumentCopySource {
                            id: source,
                            at: DocumentPoint::CURRENT,
                        },
                    },
                    StorageWrite::DocumentCopy {
                        record: child_record(retired_copy, root),
                        source: crate::types::DocumentCopySource {
                            id: source,
                            at: DocumentPoint::CURRENT,
                        },
                    },
                    StorageWrite::DocumentRetire { id: retired_copy },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let historical = storage
            .document(historical_copy, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("historical copy missing")?;
        check_eq!(historical.version, 2);
        check_eq!(historical.value.get("count"), Some(&serde_json::json!(1)));
        let current = storage
            .document(current_copy, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("current copy missing")?;
        check_eq!(current.value.get("count"), Some(&serde_json::json!(2)));
        check_eq!(
            current.value.get("rows"),
            Some(&serde_json::json!([{ "value": "base" }, { "value": "current" }]))
        );
        check!(
            storage
                .document(retired_copy, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "create-plus-retire copy must have empty lifetime"
        );

        storage
            .commit(
                &[
                    StorageWrite::DocumentChange {
                        id: source,
                        content: DocumentContent::Base {
                            version: 2,
                            value: serde_json::json!({ "count": 99, "rows": [] })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentRetire { id: source },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            storage
                .document(current_copy, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .and_then(|stored| stored.value.get("count").cloned()),
            Some(serde_json::json!(2))
        );

        let latest_source: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let latest_copy: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let latest_record = DocumentCreate {
            id: latest_source,
            kind: "copy.latest".to_string(),
            key: None,
            scope: DocumentScope::Conversation {
                conversation_id: root,
            },
            history: Some(crate::types::DocumentHistory::Latest),
            fork: Some(crate::types::DocumentFork::Current),
        };
        storage
            .commit(
                &[StorageWrite::DocumentCreate {
                    record: latest_record.clone(),
                    content: DocumentContent::Base {
                        version: 4,
                        value: serde_json::json!({ "retained": "copy" })
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        storage
            .commit(
                &[StorageWrite::DocumentCopy {
                    record: DocumentCreate {
                        id: latest_copy,
                        key: None,
                        kind: latest_record.kind.clone(),
                        scope: DocumentScope::Conversation {
                            conversation_id: child,
                        },
                        history: Some(crate::types::DocumentHistory::Latest),
                        fork: Some(crate::types::DocumentFork::Current),
                    },
                    source: crate::types::DocumentCopySource {
                        id: latest_source,
                        at: DocumentPoint::CURRENT,
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        storage
            .commit(
                &[
                    StorageWrite::DocumentChange {
                        id: latest_source,
                        content: DocumentContent::Base {
                            version: 4,
                            value: serde_json::json!({ "retained": "source-only" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentRetire { id: latest_source },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            storage
                .document(latest_copy, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| (stored.version, stored.value.get("retained").cloned())),
            Some((4, Some(serde_json::json!("copy"))))
        );

        let conflict: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        check_rejected(
            storage
                .commit(
                    &[
                        StorageWrite::DocumentCopy {
                            record: child_record(conflict, child),
                            source: crate::types::DocumentCopySource {
                                id: current_copy,
                                at: DocumentPoint::CURRENT,
                            },
                        },
                        StorageWrite::DocumentRetire { id: current_copy },
                    ],
                    &BACKGROUND_CONTEXT,
                )
                .await,
        )?;
        check!(
            storage
                .document(conflict, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "rejected copy must not exist"
        );
        check_eq!(
            storage
                .document(current_copy, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .and_then(|stored| stored.value.get("count").cloned()),
            Some(serde_json::json!(2))
        );

        let mismatch: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let mut mismatch_record = child_record(mismatch, child);
        mismatch_record.kind = "copy.mismatch".to_string();
        check_rejected(
            storage
                .commit(
                    &[StorageWrite::DocumentCopy {
                        record: mismatch_record,
                        source: crate::types::DocumentCopySource {
                            id: current_copy,
                            at: DocumentPoint::CURRENT,
                        },
                    }],
                    &BACKGROUND_CONTEXT,
                )
                .await,
        )?;
        check!(
            storage
                .document(mismatch, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "mismatched copy must not exist"
        );
        Ok(())
    })
}

fn case_18_version_transitions(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        create_root(&storage).await?;
        let id: DocumentId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        storage
            .commit(
                &[StorageWrite::DocumentCreate {
                    record: session_document(id, "session.settings"),
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
            .map_err(|e| e.to_string())?;
        storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set {
                            path: vec![crate::chord::delta::PathSegment::Key("count".to_string())],
                            value: serde_json::json!(2),
                        }],
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let migrated_at = storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Base {
                        version: 2,
                        value: serde_json::json!({ "count": 3 })
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            storage
                .document(id, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|stored| (stored.version, stored.value.get("count").cloned())),
            Some((2, Some(serde_json::json!(3))))
        );
        check_err(
            storage
                .document(id, DocumentPoint::At(migrated_at), &BACKGROUND_CONTEXT)
                .await,
            "does not retain historical content",
        )?;
        check_err(
            storage
                .commit(
                    &[StorageWrite::DocumentChange {
                        id,
                        content: DocumentContent::Delta {
                            version: 1,
                            ops: vec![Op::Set {
                                path: vec![crate::chord::delta::PathSegment::Key(
                                    "count".to_string(),
                                )],
                                value: serde_json::json!(4),
                            }],
                        },
                    }],
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "version transition requires a base",
        )?;
        check_eq!(
            storage
                .document(id, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .and_then(|stored| stored.value.get("count").cloned()),
            Some(serde_json::json!(3))
        );
        storage
            .commit(&[StorageWrite::DocumentRetire { id }], &BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check!(
            storage
                .document(id, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "retired document must read as absent"
        );
        Ok(())
    })
}

fn case_19_document_addresses(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let first: DocumentId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let conversation_doc: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task_singleton: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task_family: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task_other_kind: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let created_at = storage
            .commit(
                &[
                    StorageWrite::Task {
                        value: pending_task(task, root, "ready"),
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: first,
                            kind: "cache".to_string(),
                            key: Some("__proto__".to_string()),
                            scope: DocumentScope::Session,
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "owner": "first" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: second,
                            kind: "cache".to_string(),
                            key: Some("constructor".to_string()),
                            scope: DocumentScope::Session,
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "owner": "second" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: conversation_doc,
                            kind: "cache".to_string(),
                            key: Some("__proto__".to_string()),
                            scope: DocumentScope::Conversation {
                                conversation_id: root,
                            },
                            history: Some(crate::types::DocumentHistory::Latest),
                            fork: Some(crate::types::DocumentFork::Current),
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "owner": "conversation" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: task_singleton,
                            kind: "task.cache".to_string(),
                            key: None,
                            scope: DocumentScope::Task { task_id: task },
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "owner": "singleton" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: task_family,
                            kind: "task.cache".to_string(),
                            key: Some("member".to_string()),
                            scope: DocumentScope::Task { task_id: task },
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "owner": "family" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: task_other_kind,
                            kind: "task.other".to_string(),
                            key: None,
                            scope: DocumentScope::Task { task_id: task },
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "owner": "other" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: "cache".to_string(),
                        scope: DocumentScope::Session,
                        key: Some("__proto__".to_string()),
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(first)
        );
        let first_page = storage
            .scan_documents(
                DocumentQuery {
                    scope: DocumentScope::Session,
                    at: DocumentPoint::CURRENT,
                    kind: None,
                },
                1,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(first_page.items.len(), 1);
        let second_page = storage
            .scan_documents(
                DocumentQuery {
                    scope: DocumentScope::Session,
                    at: DocumentPoint::CURRENT,
                    kind: None,
                },
                1,
                first_page.next.clone(),
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            first_page
                .items
                .iter()
                .chain(second_page.items.iter())
                .map(|record| record.id.get())
                .collect::<Vec<_>>(),
            vec![first.get(), second.get()]
        );
        check_eq!(
            storage
                .scan_documents(
                    DocumentQuery {
                        scope: DocumentScope::Conversation {
                            conversation_id: root
                        },
                        at: DocumentPoint::CURRENT,
                        kind: None,
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|record| record.id.get())
                .collect::<Vec<_>>(),
            vec![conversation_doc.get()]
        );
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: "task.cache".to_string(),
                        scope: DocumentScope::Task { task_id: task },
                        key: None,
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(task_singleton)
        );
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: "task.cache".to_string(),
                        scope: DocumentScope::Task { task_id: task },
                        key: Some("member".to_string()),
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(task_family)
        );
        check_eq!(
            storage
                .scan_documents(
                    DocumentQuery {
                        scope: DocumentScope::Task { task_id: task },
                        at: DocumentPoint::CURRENT,
                        kind: Some("task.cache".to_string()),
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|record| record.id.get())
                .collect::<Vec<_>>(),
            vec![task_singleton.get(), task_family.get()]
        );
        check_err(
            storage
                .document(
                    task_singleton,
                    DocumentPoint::At(created_at),
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "does not retain historical content",
        )?;
        Ok(())
    })
}

fn case_20_document_lifecycle(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let first: DocumentId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let record = session_document(first, "singleton");
        storage
            .commit(
                &[StorageWrite::DocumentCreate {
                    record: record.clone(),
                    content: DocumentContent::Base {
                        version: 1,
                        value: serde_json::json!({ "value": 1 })
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_err(
            storage
                .commit(
                    &[
                        StorageWrite::DocumentCreate {
                            record: DocumentCreate {
                                id: second,
                                ..record.clone()
                            },
                            content: DocumentContent::Base {
                                version: 1,
                                value: serde_json::json!({ "value": 2 })
                                    .as_object()
                                    .cloned()
                                    .unwrap_or_default(),
                            },
                        },
                        StorageWrite::DocumentChange {
                            id: first,
                            content: DocumentContent::Delta {
                                version: 1,
                                ops: Vec::new(),
                            },
                        },
                    ],
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "already has a current incarnation",
        )?;
        check_eq!(
            storage
                .document(first, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .and_then(|stored| stored.value.get("value").cloned()),
            Some(serde_json::json!(1))
        );
        check!(
            storage
                .document(second, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "rejected create must not exist"
        );

        let empty: DocumentId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let empty_at = storage
            .commit(
                &[
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: empty,
                            kind: record.kind.clone(),
                            key: Some("empty".to_string()),
                            scope: DocumentScope::Conversation {
                                conversation_id: root,
                            },
                            history: Some(crate::types::DocumentHistory::Rewindable),
                            fork: Some(crate::types::DocumentFork::Initial),
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: JsonObject::new(),
                        },
                    },
                    StorageWrite::DocumentRetire { id: empty },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check!(
            storage
                .document(empty, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "empty-lifetime document must not read current"
        );
        check!(
            storage
                .document(empty, DocumentPoint::At(empty_at), &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "empty-lifetime document must not read at its commit"
        );
        check!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: record.kind.clone(),
                        scope: DocumentScope::Conversation {
                            conversation_id: root
                        },
                        key: Some("empty".to_string()),
                    },
                    DocumentPoint::At(empty_at),
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "empty-lifetime address must resolve to nothing"
        );
        Ok(())
    })
}

fn case_21_document_rollback(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let task: TaskId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let submission: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let document: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let task_value = pending_task(task, root, "ready");
        let submission_value = input_queued(submission, root, Some("atomic"));
        let record = session_document(document, "atomic");
        let baseline_seq = storage
            .commit(
                &[
                    StorageWrite::Task {
                        value: task_value.clone(),
                    },
                    StorageWrite::Submission {
                        value: submission_value.clone(),
                    },
                    StorageWrite::DocumentCreate {
                        record: record.clone(),
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "count": 1 })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        let entry_id: EntryId = id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let conflicting: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        check_err(
            storage
                .commit(
                    &[
                        StorageWrite::Task {
                            value: TaskRecord {
                                state: TaskState::Running {
                                    checkpoint: serde_json::json!({ "phase": "effect" }),
                                },
                                ..task_value.clone()
                            },
                        },
                        StorageWrite::Submission {
                            value: SubmissionRecord::Input {
                                id: submission,
                                conversation_id: root,
                                request_id: Some("atomic".to_string()),
                                status: InputSubmissionStatus::Unanswered {
                                    entry: None,
                                    reason: "failed".to_string(),
                                    detail: None,
                                },
                            },
                        },
                        StorageWrite::Entry {
                            value: entry(entry_id, root, "transient"),
                        },
                        StorageWrite::DocumentCreate {
                            record: DocumentCreate {
                                id: conflicting,
                                ..record.clone()
                            },
                            content: DocumentContent::Base {
                                version: 1,
                                value: serde_json::json!({ "count": 2 })
                                    .as_object()
                                    .cloned()
                                    .unwrap_or_default(),
                            },
                        },
                    ],
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "already has a current incarnation",
        )?;
        check_eq!(task_by_id(&storage, task).await?, task_value);
        check_eq!(
            storage
                .scan_tasks(
                    TaskQuery {
                        status: Some(crate::types::TaskStatus::Pending),
                        ..Default::default()
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items,
            vec![task_value]
        );
        check_eq!(
            storage
                .submission_by_request(root, "atomic", &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?,
            Some(submission_value)
        );
        check!(
            storage
                .entry(entry_id, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "transient entry must roll back"
        );
        check!(
            storage
                .document(conflicting, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .is_none(),
            "conflicting document must roll back"
        );
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: record.kind.clone(),
                        scope: record.scope.clone(),
                        key: None
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(document)
        );
        let after = storage
            .commit(
                &[StorageWrite::DocumentChange {
                    id: document,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set {
                            path: vec![crate::chord::delta::PathSegment::Key("count".to_string())],
                            value: serde_json::json!(3),
                        }],
                    },
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check!(after > baseline_seq, "rollback must advance the sequence");
        Ok(())
    })
}

fn case_22_identities_lossless(storage: Arc<dyn Storage>) -> ConformanceFuture {
    // Rust strings are UTF-8 and cannot carry lone UTF-16 surrogates, so the
    // case exercises identities that require JSON escaping and non-BMP text
    // (V17-13 §7.3 adaptation).
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let first = "quote\"backslash\\newline\n";
        let second = "emoji-😀-空白";
        let first_task: TaskId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second_task: TaskId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let first_submission: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second_submission: SubmissionId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let first_kind_doc: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second_kind_doc: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let first_key_doc: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let second_key_doc: DocumentId =
            id_from_number(storage.mint_id().await.map_err(|e| e.to_string())?);
        let mut first_task_value = pending_task(first_task, root, "ready");
        first_task_value.kind = first.to_string();
        let mut second_task_value = pending_task(second_task, root, "ready");
        second_task_value.kind = second.to_string();
        storage
            .commit(
                &[
                    StorageWrite::Task {
                        value: first_task_value,
                    },
                    StorageWrite::Task {
                        value: second_task_value,
                    },
                    StorageWrite::Submission {
                        value: input_queued(first_submission, root, Some(first)),
                    },
                    StorageWrite::Submission {
                        value: input_queued(second_submission, root, Some(second)),
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: first_kind_doc,
                            kind: first.to_string(),
                            key: None,
                            scope: DocumentScope::Session,
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "identity": "first kind" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: second_kind_doc,
                            kind: second.to_string(),
                            key: None,
                            scope: DocumentScope::Session,
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "identity": "second kind" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: first_key_doc,
                            kind: "family".to_string(),
                            key: Some(first.to_string()),
                            scope: DocumentScope::Session,
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "identity": "first key" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id: second_key_doc,
                            kind: "family".to_string(),
                            key: Some(second.to_string()),
                            scope: DocumentScope::Session,
                            history: None,
                            fork: None,
                        },
                        content: DocumentContent::Base {
                            version: 1,
                            value: serde_json::json!({ "identity": "second key" })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                        },
                    },
                ],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            storage
                .scan_tasks(
                    TaskQuery {
                        kind: Some(first.to_string()),
                        ..Default::default()
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|task| task.id.get())
                .collect::<Vec<_>>(),
            vec![first_task.get()]
        );
        check_eq!(
            storage
                .scan_tasks(
                    TaskQuery {
                        kind: Some(second.to_string()),
                        ..Default::default()
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|task| task.id.get())
                .collect::<Vec<_>>(),
            vec![second_task.get()]
        );
        check_eq!(task_by_id(&storage, first_task).await?.kind, first);
        check_eq!(task_by_id(&storage, second_task).await?.kind, second);
        check_eq!(
            storage
                .submission_by_request(root, first, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id()),
            Some(first_submission)
        );
        check_eq!(
            storage
                .submission_by_request(root, second, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id()),
            Some(second_submission)
        );
        check_eq!(
            storage
                .submission_by_request(root, first, &BACKGROUND_CONTEXT)
                .await
                .map_err(|e| e.to_string())?
                .and_then(|record| record.request_id().map(str::to_string)),
            Some(first.to_string())
        );
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: first.to_string(),
                        scope: DocumentScope::Session,
                        key: None
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(first_kind_doc)
        );
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: second.to_string(),
                        scope: DocumentScope::Session,
                        key: None
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(second_kind_doc)
        );
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: "family".to_string(),
                        scope: DocumentScope::Session,
                        key: Some(first.to_string())
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(first_key_doc)
        );
        check_eq!(
            storage
                .find_document(
                    DocumentAddress {
                        kind: "family".to_string(),
                        scope: DocumentScope::Session,
                        key: Some(second.to_string())
                    },
                    DocumentPoint::CURRENT,
                    &BACKGROUND_CONTEXT
                )
                .await
                .map_err(|e| e.to_string())?
                .map(|record| record.id),
            Some(second_key_doc)
        );
        check_eq!(
            storage
                .scan_documents(
                    DocumentQuery {
                        scope: DocumentScope::Session,
                        at: DocumentPoint::CURRENT,
                        kind: Some(first.to_string())
                    },
                    10,
                    None,
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|e| e.to_string())?
                .items
                .iter()
                .map(|record| record.id.get())
                .collect::<Vec<_>>(),
            vec![first_kind_doc.get()]
        );
        Ok(())
    })
}

fn case_23_global_id_namespace(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        let root = create_root(&storage).await?;
        let explicit: EntryId = id_from_number(100);
        storage
            .commit(
                &[StorageWrite::Entry {
                    value: entry(explicit, root, "message"),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(storage.mint_id().await.map_err(|e| e.to_string())?, 101);
        check_err(
            storage
                .commit(
                    &[StorageWrite::Task {
                        value: pending_task(id_from_number(100), root, "ready"),
                    }],
                    &BACKGROUND_CONTEXT,
                )
                .await,
            "ID 100 already belongs to entry",
        )?;
        let max_id: EntryId = id_from_number(9_007_199_254_740_991);
        storage
            .commit(
                &[StorageWrite::Entry {
                    value: entry(max_id, root, "last-id"),
                }],
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|e| e.to_string())?;
        check_err(storage.mint_id().await, "ID space is exhausted")?;
        check_err(storage.mint_id().await, "ID space is exhausted")
    })
}

fn case_24_close_rejects(storage: Arc<dyn Storage>) -> ConformanceFuture {
    Box::pin(async move {
        create_root(&storage).await?;
        storage
            .close(&BACKGROUND_CONTEXT)
            .await
            .map_err(|e| e.to_string())?;
        check_err(
            storage
                .conversation(crate::ids::ROOT_CONVERSATION_ID, &BACKGROUND_CONTEXT)
                .await,
            "closed",
        )?;
        check_err(storage.commit(&[], &BACKGROUND_CONTEXT).await, "closed")?;
        check_err(storage.mint_id().await, "closed")
    })
}

/// The complete runner-independent case list.
pub fn create_storage_conformance() -> Vec<ConformanceCase> {
    vec![
        ConformanceCase {
            name: "reserves ID 1 for the immutable root conversation",
            run: case_1_reserves_root,
        },
        ConformanceCase {
            name: "commits mixed table writes atomically and rolls all of them back on failure",
            run: case_2_mixed_commit_rollback,
        },
        ConformanceCase {
            name: "detaches retained writes and every returned record",
            run: case_3_detaches_writes,
        },
        ConformanceCase {
            name: "detaches prototype-like JSON keys without changing object prototypes",
            run: case_4_prototype_like_keys,
        },
        ConformanceCase {
            name: "indexes entries committed out of ID order",
            run: case_5_out_of_order_entries,
        },
        ConformanceCase {
            name: "continues an entry cursor below its last item after a newer commit",
            run: case_6_entry_cursor_after_newer_commit,
        },
        ConformanceCase {
            name: "paginates conversations by opaque cursor in ascending ID order",
            run: case_7_conversation_cursor,
        },
        ConformanceCase {
            name: "filters and pages conversations by durable owner edges",
            run: case_8_owner_edges,
        },
        ConformanceCase {
            name: "scans deep fork history newest-first through every ancestor cap",
            run: case_9_deep_fork_scan,
        },
        ConformanceCase {
            name: "scans tables in either ID order and continues a cursor in its order",
            run: case_10_table_order_and_cursor,
        },
        ConformanceCase {
            name: "replaces complete task records and pages filtered task scans",
            run: case_11_task_replacement_and_pages,
        },
        ConformanceCase {
            name: "stores owners and scans waiting and completing tasks by status",
            run: case_12_task_owners_and_status,
        },
        ConformanceCase {
            name: "indexes request IDs per conversation and replaces complete submission records",
            run: case_13_request_ids,
        },
        ConformanceCase {
            name: "stores passive write submissions without input-only lifecycle states",
            run: case_14_passive_submissions,
        },
        ConformanceCase {
            name: "reconstructs rewindable documents and preserves half-open incarnations",
            run: case_15_rewindable_documents,
        },
        ConformanceCase {
            name: "streams long document tails across root replacement deltas",
            run: case_16_long_tails,
        },
        ConformanceCase {
            name: "copies stored document bases independently and rejects ambiguous sources",
            run: case_17_document_copies,
        },
        ConformanceCase {
            name: "uses bases for version transitions and rejects historical reads of current-only documents",
            run: case_18_version_transitions,
        },
        ConformanceCase {
            name: "indexes logical addresses and exact-scope scans independently",
            run: case_19_document_addresses,
        },
        ConformanceCase {
            name: "keeps document lifecycle failures atomic and gives create-plus-retire an empty lifetime",
            run: case_20_document_lifecycle,
        },
        ConformanceCase {
            name: "rolls back record tables and secondary indexes when a document command fails",
            run: case_21_document_rollback,
        },
        ConformanceCase {
            name: "keeps indexed string identities lossless",
            run: case_22_identities_lossless,
        },
        ConformanceCase {
            name: "keeps one global record ID namespace and rejects exhausted ID minting",
            run: case_23_global_id_namespace,
        },
        ConformanceCase {
            name: "rejects every operation after close",
            run: case_24_close_rejects,
        },
    ]
}

/// Run every conformance case with a fresh storage per case and report all
/// failures (V17-13 §7.3: single Rust test entry instead of `describe`/`it`).
pub async fn run_storage_conformance<F, Fut>(mut make: F) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = StorageResult<Arc<dyn Storage>>>,
{
    let mut failures: Vec<String> = Vec::new();
    for case in create_storage_conformance() {
        let storage = match make().await {
            Ok(storage) => storage,
            Err(error) => {
                failures.push(format!("{}: storage setup failed: {error}", case.name));
                continue;
            }
        };
        if let Err(error) = (case.run)(Arc::clone(&storage)).await {
            failures.push(format!("{}: {error}", case.name));
        }
        let _ = storage.close(&BACKGROUND_CONTEXT).await;
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}
