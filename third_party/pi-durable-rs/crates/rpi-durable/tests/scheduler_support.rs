#![allow(dead_code)]

//! Shared fixtures for the scheduler integration tests (V17-14, worker 3a):
//! test task definitions exercising the phase/abort/migrate/join surface, and a
//! Harness opened over fresh memory storage with the **real** `TaskScheduler`.
//!
//! Upstream anchors: `packages/durable/test/harness-tasks.test.ts` and
//! `harness-tasks-recovery.test.ts` build the same `addTask`/`addHooks` fixtures;
//! here every definition is a plain [`TaskDefinition`].

use std::sync::Arc;
use tokio::sync::Notify;

use async_trait::async_trait;
use rpi_ai::models::Models;
use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context};
use rpi_durable::chord::json::JsonValue;
use rpi_durable::harness::{
    AnyTask, Extension, HarnessImpl, HarnessOptions, Migration, NextTaskState, Registry,
    RegistryImpl, RunningTask, TaskCommitFn, TaskDefinition, TaskRuntime, open_harness,
};
use rpi_durable::ids::{Conversation, ConversationId, Task, TaskId, id_from_number};
use rpi_durable::session::{
    SessionError, SessionResult, TaskCreate, TaskOwnership, Transaction, commit_fn,
};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::{JoinPolicy, Storage, TaskOutcome, TaskOutcomeError, TaskState};

pub const GENERATION: &str = "pi.generation";
pub const TOOL: &str = "pi.tool";
pub const COMPACTION: &str = "pi.compaction";

/// Reserved built-in task name placeholder (the registry check needs the names).
pub struct FakeTask(pub &'static str);

impl TaskDefinition for FakeTask {
    fn name(&self) -> &str {
        self.0
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "prepare" })
    }
}

/// Build a `TaskCommitFn` that returns `next`.
pub fn commit_next(next: NextTaskState) -> TaskCommitFn {
    Arc::new(move |_tx: &mut Transaction, _current: RunningTask| {
        let next = next.clone();
        Box::pin(async move { Ok(Some(next)) })
    })
}

fn input_u64(task: &RunningTask, key: &str) -> u64 {
    task.record
        .input
        .get(key)
        .and_then(JsonValue::as_u64)
        .unwrap_or(0)
}

/// Multi-phase task: commits a running checkpoint per phase, then completes.
/// Upstream: "continues one invocation through checkpoint progress".
pub struct CountTask;

#[async_trait]
impl TaskDefinition for CountTask {
    fn name(&self) -> &str {
        "test.count"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step", "count": 0 })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let count = task
            .checkpoint()
            .and_then(|checkpoint| checkpoint.get("count"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let total = input_u64(&task, "phases").max(1);
        let next = if count + 1 >= total {
            NextTaskState::Terminal {
                outcome: TaskOutcome::Completed {
                    result: serde_json::json!({ "count": count + 1 }),
                },
            }
        } else {
            NextTaskState::Running {
                checkpoint: serde_json::json!({ "phase": "step", "count": count + 1 }),
            }
        };
        runtime.commit(commit_next(next), context).await
    }
}
/// Task fixture that yields through a durable sleep before completing.
pub struct SleepingTask;

#[async_trait]
impl TaskDefinition for SleepingTask {
    fn name(&self) -> &str {
        "test.sleeping"
    }

    fn version(&self) -> u64 {
        1
    }

    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "sleep" })
    }

    async fn run_phase(
        &self,
        phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        if phase == "sleep" {
            return runtime
                .sleep_until(
                    runtime.now()? + 60_000.0,
                    serde_json::json!({ "phase": "after" }),
                    context,
                )
                .await;
        }
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: serde_json::json!({ "woke": true }),
                    },
                }),
                context,
            )
            .await
    }

    async fn abort(
        &self,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Aborted {
                        reason: Some("test abort".to_string()),
                        result: None,
                    },
                }),
                context,
            )
            .await
    }
}

pub struct DueSleepingTask;

#[async_trait]
impl TaskDefinition for DueSleepingTask {
    fn name(&self) -> &str {
        "test.due-sleeping"
    }

    fn version(&self) -> u64 {
        1
    }

    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "sleep" })
    }

    async fn run_phase(
        &self,
        phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        if phase == "sleep" {
            return runtime
                .sleep_until(
                    runtime.now()?,
                    serde_json::json!({ "phase": "after" }),
                    context,
                )
                .await;
        }
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: serde_json::json!({ "woke": true }),
                    },
                }),
                context,
            )
            .await
    }
}

/// Phase handler that throws (`previous.failure` path).
pub struct ThrowingTask;

#[async_trait]
impl TaskDefinition for ThrowingTask {
    fn name(&self) -> &str {
        "test.throw"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        _runtime: &dyn TaskRuntime,
        _context: &Context,
    ) -> SessionResult<()> {
        Err(SessionError::message("boom"))
    }
}

pub struct PanickingTask;

#[async_trait]
impl TaskDefinition for PanickingTask {
    fn name(&self) -> &str {
        "test.panicking"
    }

    fn version(&self) -> u64 {
        1
    }

    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }

    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        _runtime: &dyn TaskRuntime,
        _context: &Context,
    ) -> SessionResult<()> {
        panic!("task phase panic");
    }
}

pub struct BlockingTask {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

#[async_trait]
impl TaskDefinition for BlockingTask {
    fn name(&self) -> &str {
        "test.blocking"
    }

    fn version(&self) -> u64 {
        1
    }

    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "blocked" })
    }

    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        self.entered.notify_one();
        self.release.notified().await;
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: JsonValue::Null,
                    },
                }),
                context,
            )
            .await
    }
}

/// Phase handler that returns without committing (no durable progress).
pub struct NoProgressTask;

#[async_trait]
impl TaskDefinition for NoProgressTask {
    fn name(&self) -> &str {
        "test.noprogress"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        _runtime: &dyn TaskRuntime,
        _context: &Context,
    ) -> SessionResult<()> {
        Ok(())
    }
}

/// Commits a terminal outcome, then throws: the outcome must survive.
pub struct TerminalThenThrowTask;

#[async_trait]
impl TaskDefinition for TerminalThenThrowTask {
    fn name(&self) -> &str {
        "test.terminal-throw"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: serde_json::json!({ "kept": true }),
                    },
                }),
                context,
            )
            .await?;
        Err(SessionError::message("after terminal"))
    }
}

/// Waits on `input.on` with `input.policy`, then completes with `{"done":true}`.
pub struct WaitTask;

#[async_trait]
impl TaskDefinition for WaitTask {
    fn name(&self) -> &str {
        "test.wait"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "wait", "waited": false })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let waited = task
            .checkpoint()
            .and_then(|checkpoint| checkpoint.get("waited"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if waited {
            return runtime
                .commit(
                    commit_next(NextTaskState::Terminal {
                        outcome: TaskOutcome::Completed {
                            result: serde_json::json!({ "done": true }),
                        },
                    }),
                    context,
                )
                .await;
        }
        let on: Vec<TaskId> = task
            .record
            .input
            .get("on")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(JsonValue::as_u64)
                    .map(id_from_number::<Task>)
                    .collect()
            })
            .unwrap_or_default();
        let policy =
            if task.record.input.get("policy").and_then(JsonValue::as_str) == Some("failFast") {
                JoinPolicy::FailFast
            } else {
                JoinPolicy::AllSettled
            };
        runtime
            .commit(
                commit_next(NextTaskState::Waiting {
                    checkpoint: serde_json::json!({ "phase": "wait", "waited": true }),
                    on,
                    policy,
                }),
                context,
            )
            .await
    }
}

/// Completes with a `failed` outcome.
pub struct FailingTask;

#[async_trait]
impl TaskDefinition for FailingTask {
    fn name(&self) -> &str {
        "test.fail"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Failed {
                        error: TaskOutcomeError {
                            message: "failed".into(),
                            detail: None,
                        },
                        result: None,
                    },
                }),
                context,
            )
            .await
    }
}

/// Stores a memo in phase one and reads it back in phase two.
pub struct MemoTask;

#[async_trait]
impl TaskDefinition for MemoTask {
    fn name(&self) -> &str {
        "test.memo"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }
    async fn run_phase(
        &self,
        phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        match phase {
            "step" => {
                let winner = runtime.memo_set("m", serde_json::json!(7), context).await?;
                runtime
                    .commit(
                        commit_next(NextTaskState::Running {
                            checkpoint: serde_json::json!({ "phase": "second", "first": winner }),
                        }),
                        context,
                    )
                    .await
            }
            "second" => {
                let seen = runtime.memo("m", context).await?;
                runtime
                    .commit(
                        commit_next(NextTaskState::Terminal {
                            outcome: TaskOutcome::Completed {
                                result: seen.unwrap_or(JsonValue::Null),
                            },
                        }),
                        context,
                    )
                    .await
            }
            _ => Ok(()),
        }
    }
}

/// A running task that blocks in a cancellable sleep until aborted.
pub struct AbortTask;

#[async_trait]
impl TaskDefinition for AbortTask {
    fn name(&self) -> &str {
        "test.abort"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step", "count": 0 })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let count = task
            .checkpoint()
            .and_then(|checkpoint| checkpoint.get("count"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        runtime
            .commit(
                commit_next(NextTaskState::Running {
                    checkpoint: serde_json::json!({ "phase": "step", "count": count + 1 }),
                }),
                context,
            )
            .await?;
        let until = runtime.now()? + 30_000.0;
        runtime
            .sleep_until(
                until,
                serde_json::json!({ "phase": "step", "count": count + 1 }),
                context,
            )
            .await
    }

    async fn abort(
        &self,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Aborted {
                        reason: Some("aborted".into()),
                        result: None,
                    },
                }),
                context,
            )
            .await
    }
}

/// An abort handler that returns without a terminal outcome.
pub struct AbortNoOutcomeTask;

#[async_trait]
impl TaskDefinition for AbortNoOutcomeTask {
    fn name(&self) -> &str {
        "test.abort-no-outcome"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        _runtime: &dyn TaskRuntime,
        _context: &Context,
    ) -> SessionResult<()> {
        Ok(())
    }
    async fn abort(
        &self,
        _task: RunningTask,
        _runtime: &dyn TaskRuntime,
        _context: &Context,
    ) -> SessionResult<()> {
        Ok(())
    }
}

/// Version-2 definition that migrates a version-1 record; completes `{"v":2}`.
pub struct MigratingTaskV2;

#[async_trait]
impl TaskDefinition for MigratingTaskV2 {
    fn name(&self) -> &str {
        "test.migrate"
    }
    fn version(&self) -> u64 {
        2
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step", "v": 2 })
    }
    fn has_migration(&self) -> bool {
        true
    }
    fn migrate(&self, input: JsonValue, _checkpoint: JsonValue, _from: u64) -> Option<Migration> {
        Some(Migration {
            input,
            checkpoint: serde_json::json!({ "phase": "step", "v": 2 }),
        })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: serde_json::json!({ "v": 2 }),
                    },
                }),
                context,
            )
            .await
    }
}

/// Version-2 definition with no migration from version 1.
pub struct NoMigrationV2;

#[async_trait]
impl TaskDefinition for NoMigrationV2 {
    fn name(&self) -> &str {
        "test.old"
    }
    fn version(&self) -> u64 {
        2
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step", "v": 2 })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: serde_json::json!({ "v": 2 }),
                    },
                }),
                context,
            )
            .await
    }
}

/// Creates two owned children (`test.fail`, `test.abort`) and waits `failFast`.
pub struct FanoutTask;

#[async_trait]
impl TaskDefinition for FanoutTask {
    fn name(&self) -> &str {
        "test.fanout"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step", "waited": false })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let waited = task
            .checkpoint()
            .and_then(|checkpoint| checkpoint.get("waited"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if waited {
            return runtime
                .commit(
                    commit_next(NextTaskState::Terminal {
                        outcome: TaskOutcome::Completed {
                            result: serde_json::json!({ "fanout": true }),
                        },
                    }),
                    context,
                )
                .await;
        }
        let parent = task.id();
        let conversation_id = task.record.conversation_id;
        let change: TaskCommitFn = Arc::new(move |tx: &mut Transaction, _current: RunningTask| {
            Box::pin(async move {
                let failing = tx
                    .create_task(TaskCreate {
                        kind: "test.fail".to_string(),
                        version: 1,
                        input: JsonValue::Null,
                        checkpoint: serde_json::json!({ "phase": "step" }),
                        ownership: TaskOwnership::Task(parent),
                        conversation_id: Some(conversation_id),
                        background: false,
                    })
                    .await?;
                let slow = tx
                    .create_task(TaskCreate {
                        kind: "test.abort".to_string(),
                        version: 1,
                        input: JsonValue::Null,
                        checkpoint: serde_json::json!({ "phase": "step" }),
                        ownership: TaskOwnership::Task(parent),
                        conversation_id: Some(conversation_id),
                        background: false,
                    })
                    .await?;
                Ok(Some(NextTaskState::Waiting {
                    checkpoint: serde_json::json!({ "phase": "step", "waited": true }),
                    on: vec![failing, slow],
                    policy: JoinPolicy::FailFast,
                }))
            })
        });
        runtime.commit(change, context).await
    }
}

/// A definition registered later, to exercise blocked→ready on registration.
pub struct DynamicTask;

#[async_trait]
impl TaskDefinition for DynamicTask {
    fn name(&self) -> &str {
        "test.dynamic"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        serde_json::json!({ "phase": "step" })
    }
    async fn run_phase(
        &self,
        _phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        runtime
            .commit(
                commit_next(NextTaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: serde_json::json!({ "dynamic": true }),
                    },
                }),
                context,
            )
            .await
    }
}

/// A fresh Harness over memory storage with the real scheduler.
pub struct Opened {
    pub harness: Arc<HarnessImpl>,
    pub registry: Arc<RegistryImpl>,
    pub storage: Arc<dyn Storage>,
}

/// The shared background context.
pub fn context() -> Context {
    BACKGROUND_CONTEXT.clone()
}

fn builtins() -> Vec<AnyTask> {
    [GENERATION, TOOL, COMPACTION]
        .into_iter()
        .map(|name| Arc::new(FakeTask(name)) as AnyTask)
        .collect()
}

/// Open a Harness whose registry holds `tasks` under one test extension.
pub async fn open_with(tasks: Vec<AnyTask>) -> Opened {
    open_with_storage(Arc::new(MemoryStorage::new()), tasks).await
}

/// Open a Harness over `storage` whose registry holds `tasks`.
pub async fn open_with_storage(storage: Arc<dyn Storage>, tasks: Vec<AnyTask>) -> Opened {
    let registry = Arc::new(RegistryImpl::with_builtins(builtins()));
    if !tasks.is_empty() {
        let extension = Extension {
            name: "test".into(),
            tasks,
            ..Extension::default()
        };
        registry.install(extension).expect("install test extension");
    }
    let options = HarnessOptions {
        models: Models::new(None),
        registry: Arc::clone(&registry) as Arc<dyn rpi_durable::harness::RegistryReader>,
        settings: None,
        env: None,
        conversation_created: None,
        now: None,
        on_report: None,
        read_only: false,
        set_timer: None,
        on_wake: None,
    };
    let harness = open_harness(Arc::clone(&storage), options, &context())
        .await
        .expect("open harness");
    Opened {
        harness,
        registry,
        storage,
    }
}

/// All test definitions, each registered under `test.*`.
pub fn all_tasks() -> Vec<AnyTask> {
    vec![
        Arc::new(CountTask) as AnyTask,
        Arc::new(SleepingTask),
        Arc::new(DueSleepingTask),
        Arc::new(ThrowingTask),
        Arc::new(PanickingTask),
        Arc::new(NoProgressTask),
        Arc::new(TerminalThenThrowTask),
        Arc::new(WaitTask),
        Arc::new(FailingTask),
        Arc::new(MemoTask),
        Arc::new(AbortTask),
        Arc::new(AbortNoOutcomeTask),
        Arc::new(MigratingTaskV2),
        Arc::new(NoMigrationV2),
        Arc::new(FanoutTask),
        Arc::new(DynamicTask),
    ]
}

/// Create the root conversation.
pub async fn root(harness: &Arc<HarnessImpl>) -> ConversationId {
    harness
        .root(&context(), Default::default())
        .await
        .expect("root")
        .id()
}

/// One task's creation fields.
#[derive(Clone)]
pub struct Spec {
    pub kind: &'static str,
    pub version: u64,
    pub input: JsonValue,
    pub checkpoint: JsonValue,
    pub ownership: TaskOwnership,
    pub background: bool,
}

impl Spec {
    /// A conversation-owned task of `kind` with the given input.
    pub fn owned(kind: &'static str, input: JsonValue) -> Self {
        Self {
            kind,
            version: 1,
            input,
            checkpoint: serde_json::json!({ "phase": "step" }),
            ownership: TaskOwnership::Conversation(id_from_number::<Conversation>(0)),
            background: false,
        }
    }

    /// A child of `owner`.
    pub fn child(kind: &'static str, input: JsonValue, owner: TaskId) -> Self {
        Self {
            kind,
            version: 1,
            input,
            checkpoint: serde_json::json!({ "phase": "step" }),
            ownership: TaskOwnership::Task(owner),
            background: false,
        }
    }
}

/// Create one task in `conversation_id` and return its ID.
pub async fn add_task(
    harness: &Arc<HarnessImpl>,
    conversation_id: ConversationId,
    mut spec: Spec,
) -> TaskId {
    if matches!(spec.ownership, TaskOwnership::Conversation(_)) {
        spec.ownership = TaskOwnership::Conversation(conversation_id);
    }
    let change = commit_fn(move |tx: &mut Transaction| {
        let spec = spec.clone();
        Box::pin(async move {
            tx.create_task(TaskCreate {
                kind: spec.kind.to_string(),
                version: spec.version,
                input: spec.input.clone(),
                checkpoint: spec.checkpoint.clone(),
                ownership: spec.ownership,
                conversation_id: Some(conversation_id),
                background: spec.background,
            })
            .await
        })
    });
    harness
        .session()
        .commit_with(change, &context(), None)
        .await
        .expect("create task")
}

/// Create a parent task and its children in one commit (so the parent's first
/// phase already sees them), returning `(parent, children)`.
pub async fn add_family(
    harness: &Arc<HarnessImpl>,
    conversation_id: ConversationId,
    parent: Spec,
    children: Vec<Spec>,
) -> (TaskId, Vec<TaskId>) {
    let change = commit_fn(move |tx: &mut Transaction| {
        let parent = parent.clone();
        let children: Vec<Spec> = children.clone();
        Box::pin(async move {
            let parent_id = tx
                .create_task(TaskCreate {
                    kind: parent.kind.to_string(),
                    version: parent.version,
                    input: parent.input.clone(),
                    checkpoint: parent.checkpoint.clone(),
                    ownership: TaskOwnership::Conversation(conversation_id),
                    conversation_id: Some(conversation_id),
                    background: false,
                })
                .await?;
            let mut ids = Vec::new();
            for child in children {
                ids.push(
                    tx.create_task(TaskCreate {
                        kind: child.kind.to_string(),
                        version: child.version,
                        input: child.input.clone(),
                        checkpoint: child.checkpoint.clone(),
                        ownership: TaskOwnership::Task(parent_id),
                        conversation_id: Some(conversation_id),
                        background: false,
                    })
                    .await?,
                );
            }
            Ok((parent_id, ids))
        })
    });
    harness
        .session()
        .commit_with(change, &context(), None)
        .await
        .expect("create family")
}

/// Set a task's committed record directly (for recovery fixtures).
pub async fn stamp_task(harness: &Arc<HarnessImpl>, record: rpi_durable::types::TaskRecord) {
    let change = commit_fn(move |tx: &mut Transaction| {
        let record = record.clone();
        Box::pin(async move { tx.set_task(record) })
    });
    harness
        .session()
        .commit_with(change, &context(), None)
        .await
        .expect("stamp task");
}

/// The runnable checkpoint of a record, or `null` when it has an outcome.
pub fn checkpoint(record: &rpi_durable::types::TaskRecord) -> JsonValue {
    match &record.state {
        TaskState::Pending { checkpoint }
        | TaskState::Running { checkpoint }
        | TaskState::Waiting { checkpoint, .. }
        | TaskState::Sleeping { checkpoint, .. } => checkpoint.clone(),
        TaskState::Completing { .. } | TaskState::Terminal { .. } => JsonValue::Null,
    }
}

/// The terminal outcome status of a record, when terminal.
pub fn terminal_status(record: &rpi_durable::types::TaskRecord) -> Option<&'static str> {
    match &record.state {
        rpi_durable::types::TaskState::Terminal { outcome } => Some(match outcome {
            TaskOutcome::Completed { .. } => "completed",
            TaskOutcome::Failed { .. } => "failed",
            TaskOutcome::Aborted { .. } => "aborted",
            TaskOutcome::Orphaned { .. } => "orphaned",
            TaskOutcome::Faulted { .. } => "faulted",
        }),
        _ => None,
    }
}
