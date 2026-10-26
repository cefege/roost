//! Durable code-mode start, result journal, and conversation store documents.
//!
//! Start data and replay receipts are task-owned; only successful script writes
//! update the conversation-owned store.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::chord::context::Context;
use crate::chord::json::JsonObject;
use crate::documents::{DocDefinition, DocScope, DocToken, define_doc};
use crate::harness::types::{ToolExecutionApi, ToolExecutionResult};
use crate::ids::{ConversationId, TaskId};
use crate::session::{DocOwner, SessionError};
use crate::types::{DocumentFork, DocumentHistory};

pub(super) const CODEMODE_VERSION: u64 = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JournalEntry {
    pub(super) id: u32,
    pub(super) at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) issued_after: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) global: Option<GlobalReceipt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GlobalReceipt {
    pub(super) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) args: Option<JsonValue>,
    pub(super) outcome: JsonValue,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JournalState {
    #[serde(default)]
    pub(super) entries: Vec<JournalEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) first_issued: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct RunState {
    pub(super) version: u64,
    pub(super) script_hash: String,
    pub(super) fingerprint: Option<String>,
    pub(super) started_at_ms: u64,
    pub(super) seed: u64,
    pub(super) deadline_ms: Option<u64>,
    pub(super) max_output_tokens: u64,
    pub(super) tools: Vec<DeclaredTool>,
    pub(super) globals: Vec<DeclaredGlobal>,
    pub(super) store: BTreeMap<String, JsonValue>,
    pub(super) output_dir: Option<String>,
    pub(super) result: Option<ToolExecutionResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeclaredTool {
    pub(super) name: String,
    pub(super) js_name: String,
    pub(super) description: String,
    #[serde(default)]
    pub(super) structured: bool,
    #[serde(default)]
    pub(super) parameters: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeclaredGlobal {
    pub(super) name: String,
    pub(super) spread: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreState {
    values: BTreeMap<String, JsonValue>,
}

fn initial_run() -> JsonObject {
    serde_json::to_value(RunState::default())
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn initial_journal() -> JsonObject {
    serde_json::to_value(JournalState::default())
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn initial_store() -> JsonObject {
    serde_json::to_value(StoreState::default())
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

static CODEMODE_RUN_DOC: std::sync::LazyLock<DocToken> = std::sync::LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.codemode.run".to_string(),
        version: 1,
        scope: DocScope::Task,
        history: None,
        fork: None,
        family: false,
        initial: Arc::new(|_| initial_run()),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_, _, _| false)),
    })
});

static CODEMODE_JOURNAL_DOC: std::sync::LazyLock<DocToken> = std::sync::LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.codemode.journal".to_string(),
        version: 1,
        scope: DocScope::Task,
        history: None,
        fork: None,
        family: false,
        initial: Arc::new(|_| initial_journal()),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_, _, _| false)),
    })
});

static CODEMODE_STORE_DOC: std::sync::LazyLock<DocToken> = std::sync::LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.codemode.store".to_string(),
        version: 1,
        scope: DocScope::Conversation,
        history: Some(DocumentHistory::Rewindable),
        fork: Some(DocumentFork::AsOf),
        family: false,
        initial: Arc::new(|_| initial_store()),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_, _, revision| revision >= 64)),
    })
});

pub(super) async fn load_or_create_run(
    api: &dyn ToolExecutionApi,
    task_id: TaskId,
    conversation_id: ConversationId,
    script_hash: &str,
    now_ms: u64,
    seed: u64,
    deadline_ms: Option<u64>,
    max_output_tokens: u64,
    tools: Vec<DeclaredTool>,
    globals: Vec<DeclaredGlobal>,
    context: &Context,
) -> Result<(RunState, JournalState, bool), crate::harness::types::ToolExecuteError> {
    let script_hash = script_hash.to_string();
    let state = api
        .commit(
            Arc::new(move |tx| {
                let script_hash = script_hash.clone();
                let tools = tools.clone();
                let globals = globals.clone();
                Box::pin(async move {
                    let run_doc = tx
                        .doc(&CODEMODE_RUN_DOC, DocOwner::Task(task_id), None)
                        .await?;
                    let mut run: RunState = crate::harness::types::read_state(&run_doc)?;
                    let rerun = run.version != 0;
                    if !rerun {
                        let store_doc = tx
                            .doc(
                                &CODEMODE_STORE_DOC,
                                DocOwner::Conversation(conversation_id),
                                None,
                            )
                            .await?;
                        let store: StoreState = crate::harness::types::read_state(&store_doc)?;
                        run = RunState {
                            version: CODEMODE_VERSION,
                            script_hash,
                            fingerprint: None,
                            started_at_ms: now_ms,
                            seed,
                            deadline_ms,
                            max_output_tokens,
                            tools,
                            globals,
                            store: store.values,
                            output_dir: None,
                            result: None,
                        };
                        crate::harness::types::write_state(&run_doc, &run)?;
                    }
                    let journal_doc = tx
                        .doc(&CODEMODE_JOURNAL_DOC, DocOwner::Task(task_id), None)
                        .await?;
                    let journal: JournalState = crate::harness::types::read_state(&journal_doc)?;
                    serde_json::to_value((run, journal, rerun))
                        .map_err(|error| SessionError::message(error.to_string()))
                })
            }),
            context,
        )
        .await
        .map_err(super::tool_error)?;
    serde_json::from_value(state).map_err(super::tool_error)
}

pub(super) async fn persist_fingerprint(
    api: &dyn ToolExecutionApi,
    task_id: TaskId,
    fingerprint: String,
    context: &Context,
) -> Result<(), SessionError> {
    api.commit(Arc::new(move |tx| {
        let fingerprint = fingerprint.clone();
        Box::pin(async move {
            let doc = tx.doc(&CODEMODE_RUN_DOC, DocOwner::Task(task_id), None).await?;
            let mut run: RunState = crate::harness::types::read_state(&doc)?;
            match &run.fingerprint {
                Some(recorded) if recorded != &fingerprint => {
                    return Err(SessionError::message(format!(
                        "The script cannot resume: the sandbox changed since it started ({recorded} -> {fingerprint})."
                    )));
                }
                Some(_) => {}
                None => {
                    run.fingerprint = Some(fingerprint);
                    crate::harness::types::write_state(&doc, &run)?;
                }
            }
            Ok(JsonValue::Null)
        })
    }), context).await.map(|_| ())
}

pub(super) async fn persist_journal_batch(
    api: &dyn ToolExecutionApi,
    task_id: TaskId,
    entries: Vec<JournalEntry>,
    acknowledgements: Vec<(u64, u32)>,
    context: &Context,
) -> Result<(), SessionError> {
    api.commit(
        Arc::new(move |tx| {
            let entries = entries.clone();
            let acknowledgements = acknowledgements.clone();
            Box::pin(async move {
                let doc = tx
                    .doc(&CODEMODE_JOURNAL_DOC, DocOwner::Task(task_id), None)
                    .await?;
                let mut journal: JournalState = crate::harness::types::read_state(&doc)?;
                for (sequence, issued) in acknowledgements {
                    if sequence == 0 {
                        match journal.first_issued {
                            Some(recorded) if recorded != issued => {
                                return Err(SessionError::message(
                                    "Code-mode first issued count changed",
                                ));
                            }
                            Some(_) => {}
                            None => journal.first_issued = Some(issued),
                        }
                        continue;
                    }
                    let index = usize::try_from(sequence - 1).unwrap_or(usize::MAX);
                    let Some(entry) = journal.entries.get_mut(index) else {
                        return Err(SessionError::message(
                            "Code-mode journal acknowledgement has no result",
                        ));
                    };
                    match entry.issued_after {
                        Some(recorded) if recorded != issued => {
                            return Err(SessionError::message(
                                "Code-mode issued count changed after a result",
                            ));
                        }
                        Some(_) => {}
                        None => entry.issued_after = Some(issued),
                    }
                }
                for entry in entries {
                    if journal
                        .entries
                        .iter()
                        .any(|recorded| recorded.id == entry.id)
                    {
                        return Err(SessionError::message(
                            "Code-mode journal contains a duplicate call id",
                        ));
                    }
                    journal.entries.push(entry);
                }
                crate::harness::types::write_state(&doc, &journal)?;
                Ok(JsonValue::Null)
            })
        }),
        context,
    )
    .await
    .map(|_| ())
}

pub(super) async fn load_output_directory(
    api: &dyn ToolExecutionApi,
    task_id: TaskId,
    context: &Context,
) -> Result<Option<String>, crate::harness::types::ToolExecuteError> {
    let state = api
        .commit(
            Arc::new(move |tx| {
                Box::pin(async move {
                    let doc = tx
                        .doc(&CODEMODE_RUN_DOC, DocOwner::Task(task_id), None)
                        .await?;
                    let run: RunState = crate::harness::types::read_state(&doc)?;
                    serde_json::to_value(run.output_dir)
                        .map_err(|error| SessionError::message(error.to_string()))
                })
            }),
            context,
        )
        .await
        .map_err(super::tool_error)?;
    serde_json::from_value(state).map_err(super::tool_error)
}

pub(super) async fn persist_output_directory(
    api: &dyn ToolExecutionApi,
    task_id: TaskId,
    output_dir: String,
    context: &Context,
) -> Result<String, crate::harness::types::ToolExecuteError> {
    let state = api
        .commit(
            Arc::new(move |tx| {
                let output_dir = output_dir.clone();
                Box::pin(async move {
                    let doc = tx
                        .doc(&CODEMODE_RUN_DOC, DocOwner::Task(task_id), None)
                        .await?;
                    let mut run: RunState = crate::harness::types::read_state(&doc)?;
                    run.output_dir.get_or_insert(output_dir);
                    let recorded = run.output_dir.clone().unwrap_or_default();
                    crate::harness::types::write_state(&doc, &run)?;
                    serde_json::to_value(recorded)
                        .map_err(|error| SessionError::message(error.to_string()))
                })
            }),
            context,
        )
        .await
        .map_err(super::tool_error)?;
    serde_json::from_value(state).map_err(super::tool_error)
}

pub(super) async fn persist_completion(
    api: &dyn ToolExecutionApi,
    task_id: TaskId,
    conversation_id: ConversationId,
    result: ToolExecutionResult,
    store: Option<BTreeMap<String, JsonValue>>,
    context: Context,
) -> Result<ToolExecutionResult, crate::harness::types::ToolExecuteError> {
    let saved = api
        .commit(
            Arc::new(move |tx| {
                let result = result.clone();
                let store = store.clone();
                Box::pin(async move {
                    let run_doc = tx
                        .doc(&CODEMODE_RUN_DOC, DocOwner::Task(task_id), None)
                        .await?;
                    let mut run: RunState = crate::harness::types::read_state(&run_doc)?;
                    if run.result.is_none() {
                        run.result = Some(result.clone());
                        crate::harness::types::write_state(&run_doc, &run)?;
                        if let Some(store) = store {
                            let store_doc = tx
                                .doc(
                                    &CODEMODE_STORE_DOC,
                                    DocOwner::Conversation(conversation_id),
                                    None,
                                )
                                .await?;
                            let mut store_state: StoreState =
                                crate::harness::types::read_state(&store_doc)?;
                            store_state.values = store;
                            crate::harness::types::write_state(&store_doc, &store_state)?;
                        }
                    }
                    serde_json::to_value(run.result.unwrap_or(result))
                        .map_err(|error| SessionError::message(error.to_string()))
                })
            }),
            &context,
        )
        .await
        .map_err(super::tool_error)?;
    serde_json::from_value(saved).map_err(super::tool_error)
}
