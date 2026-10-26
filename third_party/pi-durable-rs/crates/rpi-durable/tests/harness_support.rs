#![allow(dead_code)]

//! Shared support for the harness integration tests (V17-14): a recording task
//! scheduler double, harness construction over memory storage, and submission
//! helpers. The scheduler/generation pipeline itself is worker 3a's domain; the
//! double records run starts and performs the durable `live.run` bookkeeping the
//! boundary path needs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rpi_ai::models::Models;
use rpi_ai::types::UserContent;
use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context};
use rpi_durable::chord::json::JsonValue;
use rpi_durable::harness::{
    AnyTask, ConversationInit, HarnessImpl, HarnessOptions, InboxItem, InputSubmissionDraft,
    LiveRun, LiveState, QueueMode, QueueModes, RegistryImpl, SchedulerDeps, SchedulerFactory,
    SchedulerService, Scheduling, SettledTask, SubmissionDraft, TaskAbort, TaskDefinition,
    TaskInspection, WhenBusy, WriteSubmissionDraft, entry_to_json, open_harness_with_scheduler,
};
use rpi_durable::ids::{ConversationId, SubmissionId, TaskId};
use rpi_durable::session::{
    DocDraft, EntryDraft, SessionError, SessionResult, TaskCreate, TaskOwnership, Transaction,
};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::Storage;

/// Built-in task name of the generation pipeline.
pub const GENERATION: &str = "pi.generation";
/// Built-in task name of tool execution.
pub const TOOL: &str = "pi.tool";
/// Built-in task name of compaction.
pub const COMPACTION: &str = "pi.compaction";

/// A minimal built-in task definition so the registry satisfies `open_harness`'s
/// built-in check (worker 3a supplies the real definitions).
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

/// Recording task scheduler double: `resume`/`start_run`/`compact`/`abort` are
/// recorded; the remaining scheduler-backed operations are inert.
#[derive(Default)]
pub struct RecordingScheduler {
    /// Whether `resume()` was called.
    pub resumed: AtomicBool,
    /// `(conversation, inputs)` of every `start_run` call.
    pub started: Mutex<Vec<(ConversationId, Vec<SubmissionId>)>>,
    /// Task IDs passed to `abort`.
    pub aborted: Mutex<Vec<TaskId>>,
}

impl RecordingScheduler {
    /// Create an empty recording scheduler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every `start_run` call so far.
    pub fn started_runs(&self) -> Vec<(ConversationId, Vec<SubmissionId>)> {
        self.started.lock().expect("started").clone()
    }
}

fn read_live(live: &DocDraft) -> SessionResult<LiveState> {
    let value = live
        .read()
        .map_err(|error| SessionError::message(error.to_string()))?;
    serde_json::from_value(value).map_err(|error| SessionError::message(error.to_string()))
}

fn write_live(live: &DocDraft, state: &LiveState) -> SessionResult<()> {
    let value =
        serde_json::to_value(state).map_err(|error| SessionError::message(error.to_string()))?;
    let mut guard = live
        .write()
        .map_err(|error| SessionError::message(error.to_string()))?;
    *guard = value;
    Ok(())
}

async fn create_owned_task(
    tx: &mut rpi_durable::session::Transaction,
    conversation_id: ConversationId,
    kind: &str,
) -> SessionResult<TaskId> {
    tx.create_task(TaskCreate {
        kind: kind.to_string(),
        version: 1,
        input: JsonValue::Null,
        checkpoint: serde_json::json!({ "phase": "prepare" }),
        ownership: TaskOwnership::Conversation(conversation_id),
        conversation_id: Some(conversation_id),
        background: false,
    })
    .await
}

#[async_trait]
impl SchedulerService for RecordingScheduler {
    fn resume(&self) {
        self.resumed.store(true, Ordering::SeqCst);
    }
    fn wake(&self) -> rpi_durable::harness::Wake {
        rpi_durable::harness::Wake {
            active: false,
            at: None,
        }
    }

    async fn open(&self, _context: &Context) -> SessionResult<()> {
        Ok(())
    }

    async fn join(&self) -> SessionResult<()> {
        Ok(())
    }

    async fn start_run(
        &self,
        tx: &mut rpi_durable::session::Transaction,
        conversation_id: ConversationId,
        live: &DocDraft,
        inputs: Vec<SubmissionId>,
    ) -> SessionResult<()> {
        let task_id = create_owned_task(tx, conversation_id, GENERATION).await?;
        let mut state = read_live(live)?;
        state.run = Some(LiveRun {
            task_id,
            inputs: inputs.clone(),
        });
        write_live(live, &state)?;
        self.started
            .lock()
            .expect("started")
            .push((conversation_id, inputs));
        Ok(())
    }

    async fn compact(
        &self,
        tx: &mut rpi_durable::session::Transaction,
        conversation_id: ConversationId,
        _instructions: Option<String>,
    ) -> SessionResult<TaskId> {
        create_owned_task(tx, conversation_id, COMPACTION).await
    }

    async fn abort(&self, id: TaskId, _context: &Context) -> SessionResult<TaskAbort> {
        self.aborted.lock().expect("aborted").push(id);
        Ok(TaskAbort::Marked)
    }

    async fn abort_conversation(
        &self,
        _conversation_id: ConversationId,
        _background: bool,
        _context: &Context,
    ) -> SessionResult<()> {
        Ok(())
    }

    async fn wait_for_task(
        &self,
        _id: TaskId,
        _context: &Context,
    ) -> SessionResult<SettledTask<JsonValue>> {
        Err(SessionError::message(
            "wait_for_task is not implemented by the test double",
        ))
    }

    async fn wait_for_idle(
        &self,
        _conversation_id: Option<ConversationId>,
        _context: &Context,
    ) -> SessionResult<()> {
        Ok(())
    }

    async fn inspect(
        &self,
        _snapshot: Arc<dyn rpi_durable::harness::RegistrySnapshot>,
    ) -> SessionResult<(Scheduling, Vec<TaskInspection>)> {
        Ok((Scheduling::Paused, Vec::new()))
    }
}

/// The shared background context.
pub fn context() -> Context {
    BACKGROUND_CONTEXT.clone()
}

/// Harness options with a registry holding the three reserved built-in names.
pub fn options() -> HarnessOptions {
    let builtins: Vec<AnyTask> = [GENERATION, TOOL, COMPACTION]
        .into_iter()
        .map(|name| Arc::new(FakeTask(name)) as AnyTask)
        .collect();
    HarnessOptions {
        models: Models::new(None),
        registry: Arc::new(RegistryImpl::with_builtins(builtins)),
        settings: None,
        env: None,
        conversation_created: None,
        now: None,
        on_report: None,
        read_only: false,
        set_timer: None,
        on_wake: None,
    }
}

/// Open a Harness over fresh memory storage with the recording scheduler.
pub async fn open_recording() -> (Arc<HarnessImpl>, Arc<RecordingScheduler>) {
    open_recording_with(Arc::new(MemoryStorage::new())).await
}

/// Open a Harness over `storage` with the recording scheduler.
pub async fn open_recording_with(
    storage: Arc<dyn Storage>,
) -> (Arc<HarnessImpl>, Arc<RecordingScheduler>) {
    let scheduler = Arc::new(RecordingScheduler::new());
    let factory: SchedulerFactory = {
        let scheduler = Arc::clone(&scheduler);
        Arc::new(move |_deps: SchedulerDeps| Arc::clone(&scheduler) as Arc<dyn SchedulerService>)
    };
    let harness = open_harness_with_scheduler(storage, options(), factory, &context())
        .await
        .expect("open harness");
    (harness, scheduler)
}

/// Default queue modes (one-at-a-time).
pub fn modes() -> QueueModes {
    QueueModes {
        steering_mode: QueueMode::OneAtATime,
        follow_up_mode: QueueMode::OneAtATime,
    }
}

/// Clear `live.run` so a conversation reads as idle (a test stand-in for a run
/// ending).
pub async fn clear_run(harness: &Arc<HarnessImpl>, conversation_id: ConversationId) {
    let change = rpi_durable::session::commit_fn(move |tx| {
        Box::pin(async move {
            let live = tx
                .doc(
                    &rpi_durable::harness::LIVE_DOC,
                    rpi_durable::session::DocOwner::Conversation(conversation_id),
                    None,
                )
                .await?;
            let mut state = read_live(&live)?;
            state.run = None;
            write_live(&live, &state)?;
            Ok(())
        })
    });
    harness
        .session()
        .commit(change, &context())
        .await
        .expect("clear run");
}

/// A conversation initializer that appends one marker entry.
pub fn init_append(kind: &'static str) -> ConversationInit {
    Arc::new(move |tx: &mut Transaction, id: ConversationId| {
        Box::pin(async move {
            let mut entry = EntryDraft::new(kind);
            entry.data = Some(serde_json::json!({ "initialized": true }));
            tx.append_entry(id, entry).await?;
            Ok(())
        })
    })
}

/// An input submission draft.
pub fn input(
    content: &str,
    when_busy: Option<WhenBusy>,
    request_id: Option<&str>,
) -> SubmissionDraft {
    SubmissionDraft::Input(InputSubmissionDraft {
        request_id: request_id.map(str::to_string),
        content: UserContent::Text(content.to_string()),
        when_busy,
    })
}

/// A passive write submission draft.
pub fn write(entry: EntryDraft, request_id: Option<&str>) -> SubmissionDraft {
    SubmissionDraft::Write(WriteSubmissionDraft {
        request_id: request_id.map(str::to_string),
        entry,
    })
}

/// A queued inbox item for the given submission.
pub fn inbox_write(id: SubmissionId, entry: &EntryDraft) -> InboxItem {
    InboxItem::Write {
        id,
        entry: entry_to_json(entry),
    }
}

/// A queued steer item.
pub fn inbox_steer(id: SubmissionId, content: &str) -> InboxItem {
    InboxItem::Steer {
        id,
        content: UserContent::Text(content.to_string()),
    }
}

/// A queued follow-up item.
pub fn inbox_follow_up(id: SubmissionId, content: &str) -> InboxItem {
    InboxItem::FollowUp {
        id,
        content: UserContent::Text(content.to_string()),
    }
}
