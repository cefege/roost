//! Fork, sleep/wake, and task-ownership oracle scenarios.
//! Called by `queue_tasks` and built from its shared harness helpers.
//! Scenario work uses real durable task and document APIs.
use super::*;
/// Exercise independent as-of, current, and initial document fork policies.
pub async fn fork_document_policies() -> ScenarioResult<Value> {
    let as_of = doc(
        "oracle.fork.as-of",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
    );
    let current = doc(
        "oracle.fork.current",
        DocumentHistory::Latest,
        DocumentFork::Current,
    );
    let initial = doc(
        "oracle.fork.initial",
        DocumentHistory::Latest,
        DocumentFork::Initial,
    );
    let (models, _) = OracleProvider::new([]);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    let harness = open_harness(
        Arc::new(MemoryStorage::new()),
        options(models, registry),
        context(),
    )
    .await?;
    let parent = harness.root(context(), Default::default()).await?;
    let parent_id = parent.id();
    let point_tokens = [as_of.clone(), current.clone(), initial.clone()];
    let point = parent
        .commit(
            commit_fn(move |tx| {
                let tokens = point_tokens.clone();
                Box::pin(async move {
                    let point = tx
                        .append_entry(parent_id, EntryDraft::new("oracle.point"))
                        .await?
                        .id;
                    for (token, value) in [
                        (&tokens[0], "at-fork"),
                        (&tokens[1], "at-fork"),
                        (&tokens[2], "parent-only"),
                    ] {
                        set_value(
                            &tx.doc(token, DocOwner::Conversation(parent_id), None)
                                .await?,
                            value,
                        )
                        .map_err(|error| rpi_durable::SessionError::message(error.to_string()))?;
                    }
                    Ok(point)
                })
            }),
            context(),
        )
        .await?;
    let later_tokens = [as_of.clone(), current.clone()];
    parent
        .commit(
            commit_fn(move |tx| {
                let tokens = later_tokens.clone();
                Box::pin(async move {
                    set_value(
                        &tx.doc(&tokens[0], DocOwner::Conversation(parent_id), None)
                            .await?,
                        "later",
                    )
                    .map_err(|error| rpi_durable::SessionError::message(error.to_string()))?;
                    set_value(
                        &tx.doc(&tokens[1], DocOwner::Conversation(parent_id), None)
                            .await?,
                        "current-at-fork-time",
                    )
                    .map_err(|error| rpi_durable::SessionError::message(error.to_string()))?;
                    Ok(())
                })
            }),
            context(),
        )
        .await?;
    let child = parent
        .fork(
            point,
            rpi_durable::harness::ConversationCreateOptions {
                ownership: rpi_durable::session::ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await?;
    let as_of_value = harness
        .session()
        .snapshot(&as_of, DocOwner::Conversation(child.id()), context())
        .await?;
    let current_value = harness
        .session()
        .snapshot(&current, DocOwner::Conversation(child.id()), context())
        .await?;
    let initial_value = harness
        .session()
        .snapshot(&initial, DocOwner::Conversation(child.id()), context())
        .await?;
    let parent_value = harness
        .session()
        .snapshot(&as_of, DocOwner::Conversation(parent_id), context())
        .await?;
    let entries = child
        .entries(
            rpi_durable::types::EntryQuery {
                conversation_id: child.id(),
                min_entry_id: None,
                max_entry_id: None,
                order: Some(rpi_durable::types::ScanOrder::Ascending),
            },
            1_000,
            None,
            context(),
        )
        .await?
        .items;
    let mut trace = json!({"name":"fork-document-policies", "asOf":as_of_value, "current":current_value,
        "parentAsOf":parent_value, "childHistory":entries});
    if let Some(initial) = initial_value {
        trace
            .as_object_mut()
            .ok_or("fork trace was not an object")?
            .insert("initial".to_string(), Value::Object(initial));
    }
    harness.close(context()).await?;
    Ok(trace)
}

struct NapTask {
    after_runs: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskDefinition for NapTask {
    fn name(&self) -> &str {
        "oracle.nap"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        json!({"phase":"start"})
    }
    async fn run_phase(
        &self,
        phase: &str,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        ctx: &Context,
    ) -> rpi_durable::SessionResult<()> {
        match phase {
            "start" => {
                runtime
                    .sleep_until(runtime.now()? + 60_000.0, json!({"phase":"after"}), ctx)
                    .await
            }
            "after" => {
                self.after_runs.fetch_add(1, Ordering::SeqCst);
                let settled_at = runtime.now()?;
                runtime
                    .commit(
                        Arc::new(move |_tx, _task| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed {
                                        result: json!(settled_at),
                                    },
                                }))
                            })
                        }),
                        ctx,
                    )
                    .await
            }
            phase => Err(rpi_durable::SessionError::message(format!(
                "unknown oracle.nap phase {phase}"
            ))),
        }
    }
    async fn abort(
        &self,
        _task: RunningTask,
        runtime: &dyn TaskRuntime,
        ctx: &Context,
    ) -> rpi_durable::SessionResult<()> {
        runtime
            .commit(
                Arc::new(|_tx, _task| {
                    Box::pin(async move {
                        Ok(Some(NextTaskState::Terminal {
                            outcome: TaskOutcome::Aborted {
                                reason: None,
                                result: None,
                            },
                        }))
                    })
                }),
                ctx,
            )
            .await
    }
}

/// Persist and wake an actual sleeping task under an injected deterministic clock.
pub async fn durable_sleep_wake() -> ScenarioResult<Value> {
    let clock = Arc::new(AtomicU64::new(1_000));
    let clock_for_harness = Arc::clone(&clock);
    let after_runs = Arc::new(AtomicUsize::new(0));
    let mut tasks = builtin_tasks();
    tasks.push(Arc::new(NapTask {
        after_runs: Arc::clone(&after_runs),
    }) as AnyTask);
    let registry = Arc::new(RegistryImpl::with_builtins(tasks));
    let (models, _) = OracleProvider::new([]);
    let mut harness_options: HarnessOptions = options(models, registry);
    harness_options.now = Some(Arc::new(move || {
        clock_for_harness.load(Ordering::SeqCst) as f64
    }));
    let harness = open_harness(Arc::new(MemoryStorage::new()), harness_options, context()).await?;
    let root = harness.root(context(), Default::default()).await?;
    let root_id = root.id();
    let task_id = root
        .commit(
            commit_fn(move |tx| {
                Box::pin(async move {
                    tx.create_task(TaskCreate {
                        kind: "oracle.nap".into(),
                        version: 1,
                        input: JsonValue::Null,
                        checkpoint: json!({"phase":"start"}),
                        ownership: TaskOwnership::Conversation(root_id),
                        conversation_id: Some(root_id),
                        background: false,
                    })
                    .await
                })
            }),
            context(),
        )
        .await?;
    harness.resume()?;
    let asleep = wait_for_task(&harness, task_id, |state| {
        matches!(state, TaskState::Sleeping { .. })
    })
    .await?;
    let early = harness.wake()?;
    let early_wake = wake_json(early);
    clock.store(61_000, Ordering::SeqCst);
    let due = harness.wake()?;
    let due_wake = wake_json(due);
    harness.resume()?;
    let settled = harness.wait_for_task(task_id, context()).await?;
    let trace = json!({"name":"durable-sleep-wake", "asleep":asleep.state, "earlyWake":early_wake,
        "dueWake":due_wake, "settled":settled.record.state, "afterRuns":after_runs.load(Ordering::SeqCst)});
    harness.close(context()).await?;
    Ok(trace)
}
fn wake_json(wake: rpi_durable::harness::Wake) -> Value {
    let mut value = serde_json::Map::new();
    value.insert("active".to_string(), Value::Bool(wake.active));
    if let Some(at) = wake.at {
        value.insert("at".to_string(), json!(at));
    }
    Value::Object(value)
}

async fn wait_for_task<F>(
    harness: &Arc<rpi_durable::harness::HarnessImpl>,
    task_id: rpi_durable::ids::TaskId,
    predicate: F,
) -> ScenarioResult<rpi_durable::types::TaskRecord>
where
    F: Fn(&TaskState) -> bool,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = harness.get_task(task_id, context()).await? {
                if predicate(&record.state) {
                    return Ok(record);
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .map_err(|_| "timed out waiting for durable task state")?
}

struct HeldTask {
    started: Arc<Mutex<BTreeSet<String>>>,
    both_started: Arc<Notify>,
    release: Arc<Semaphore>,
    owner_abort_release: Arc<Semaphore>,
}

#[async_trait]
impl TaskDefinition for HeldTask {
    fn name(&self) -> &str {
        "oracle.held"
    }
    fn version(&self) -> u64 {
        1
    }
    fn initial(&self) -> JsonValue {
        json!({"phase":"wait"})
    }
    async fn run_phase(
        &self,
        _phase: &str,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        _ctx: &Context,
    ) -> rpi_durable::SessionResult<()> {
        let name = task
            .record
            .input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Ok(mut started) = self.started.lock() {
            started.insert(name);
            if started.len() == 2 {
                self.both_started.notify_one();
            }
        }
        let signal = runtime.signal();
        tokio::select! {
            _ = signal.cancelled() => Err(rpi_durable::SessionError::message("task aborted")),
            _ = self.release.acquire() => Ok(()),
        }
    }
    async fn abort(
        &self,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        ctx: &Context,
    ) -> rpi_durable::SessionResult<()> {
        if task.record.input.get("name").and_then(Value::as_str) == Some("owner") {
            let _permit = self
                .owner_abort_release
                .acquire()
                .await
                .map_err(|error| rpi_durable::SessionError::message(error.to_string()))?;
        }
        runtime
            .commit(
                Arc::new(|_tx, _task| {
                    Box::pin(async move {
                        Ok(Some(NextTaskState::Terminal {
                            outcome: TaskOutcome::Aborted {
                                reason: None,
                                result: None,
                            },
                        }))
                    })
                }),
                ctx,
            )
            .await
    }
}

/// Abort a foreground owner and capture its task-owned child cancellation.
pub async fn generic_task_ownership_abort() -> ScenarioResult<Value> {
    let started = Arc::new(Mutex::new(BTreeSet::new()));
    let both_started = Arc::new(Notify::new());
    let release = Arc::new(Semaphore::new(0));
    let owner_abort_release = Arc::new(Semaphore::new(0));
    let mut tasks = builtin_tasks();
    tasks.push(Arc::new(HeldTask {
        started: Arc::clone(&started),
        both_started: Arc::clone(&both_started),
        release: Arc::clone(&release),
        owner_abort_release: Arc::clone(&owner_abort_release),
    }) as AnyTask);
    let registry = Arc::new(RegistryImpl::with_builtins(tasks));
    let (models, _) = OracleProvider::new([]);
    let harness = open_harness(
        Arc::new(MemoryStorage::new()),
        options(models, registry),
        context(),
    )
    .await?;
    let root = harness.root(context(), Default::default()).await?;
    let root_id = root.id();
    let (owner, child) = root
        .commit(
            commit_fn(move |tx| {
                Box::pin(async move {
                    let owner = tx
                        .create_task(TaskCreate {
                            kind: "oracle.held".into(),
                            version: 1,
                            input: json!({"name":"owner"}),
                            checkpoint: json!({"phase":"wait"}),
                            ownership: TaskOwnership::Conversation(root_id),
                            conversation_id: Some(root_id),
                            background: false,
                        })
                        .await?;
                    let child = tx
                        .create_task(TaskCreate {
                            kind: "oracle.held".into(),
                            version: 1,
                            input: json!({"name":"child"}),
                            checkpoint: json!({"phase":"wait"}),
                            ownership: TaskOwnership::Task(owner),
                            conversation_id: None,
                            background: false,
                        })
                        .await?;
                    Ok((owner, child))
                })
            }),
            context(),
        )
        .await?;
    let both_started_notice = both_started.notified();
    harness.resume()?;
    tokio::time::timeout(Duration::from_secs(5), both_started_notice)
        .await
        .map_err(|_| "timed out waiting for both owned tasks to start")?;
    let abort_result = harness.abort_task(owner, context()).await?;
    let child_record = wait_for_task(&harness, child, |state| {
        matches!(
            state,
            TaskState::Terminal {
                outcome: TaskOutcome::Aborted { .. },
            }
        )
    })
    .await?;
    let owner_record = harness
        .get_task(owner, context())
        .await?
        .ok_or("owner task record missing")?;
    let started: Vec<String> = started
        .lock()
        .map_err(|_| "started-task capture poisoned")?
        .iter()
        .cloned()
        .collect();
    owner_abort_release.add_permits(1);
    let _owner_settled = wait_for_task(&harness, owner, |state| {
        matches!(
            state,
            TaskState::Terminal {
                outcome: TaskOutcome::Aborted { .. },
            }
        )
    })
    .await?;
    release.add_permits(2);
    let abort_result = match abort_result {
        rpi_durable::harness::TaskAbort::Marked => "marked",
        rpi_durable::harness::TaskAbort::Terminal => "terminal",
    };
    let trace = json!({"name":"generic-task-ownership-abort", "abortResult":abort_result,
        "owner":owner_record, "child":child_record, "started":started});
    harness.close(context()).await?;
    Ok(trace)
}
