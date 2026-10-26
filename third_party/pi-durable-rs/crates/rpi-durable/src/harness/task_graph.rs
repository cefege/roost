//! Live task graph mount (port of `packages/durable/src/harness/task-graph.ts`
//! @ `abe508e1b`).
//!
//! The task graph is a Chord replicated state over every live (non-terminal)
//! task: its durable status without checkpoint/outcome payloads, its owner edge,
//! and the conversations it owns in ID order. The mount is built on the Session
//! line by its first observer and dropped with its last, advancing from the
//! Session's commit publications.
//!
//! Rust shape note (registered V17-14 §7.3): upstream applies a hand-built `Op`
//! batch; the Rust port recomputes the changed node and publishes
//! `diff_json(previous, next)` (the tracker's structural-diff shape). A commit
//! that changes nothing publishes no revision.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::chord::context::{Context, without_abort_signal};
use crate::chord::delta::diff_json;
use crate::chord::json::JsonValue;
use crate::chord::state::AttachedReplicatedState;
use crate::ids::{ConversationId, TaskId};
use crate::session::observation::{CommittedStateSource, CommittedWatch};
use crate::session::{CommitChange, CommitPublication, SessionError, SessionImpl, SessionResult};
use crate::types::{
    ConversationQuery, ConversationRecord, Page, Storage, StorageWrite, TaskOutcome, TaskQuery,
    TaskRecord, TaskState, TaskStatus,
};

/// A live task's durable status without its checkpoint and outcome payloads
/// (spec §9.5).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TaskGraphState {
    /// Eligible for scheduling.
    Pending {
        /// Current checkpoint phase.
        phase: String,
    },
    /// Reserved by an invocation.
    Running {
        /// Current checkpoint phase.
        phase: String,
    },
    /// Parked on other tasks.
    Waiting {
        /// Current checkpoint phase.
        phase: String,
        /// Waited task IDs.
        on: Vec<TaskId>,
        /// How the wait reacts to failures.
        policy: crate::types::JoinPolicy,
    },
    /// Asleep until the Harness clock reaches `until`.
    Sleeping {
        /// Current checkpoint phase.
        phase: String,
        /// Deadline on the Harness clock, in epoch milliseconds.
        until: f64,
    },
    /// Outcome held until ordinary owned work drains.
    Completing {
        /// Held outcome status.
        outcome: String,
    },
}

/// One live task of the graph.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraphNode {
    /// Task identity.
    pub id: TaskId,
    /// Registered task kind.
    pub kind: String,
    /// Conversation the task lives in.
    pub conversation_id: ConversationId,
    /// Owning task of a child task; absent for a conversation-owned task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TaskId>,
    /// Whether the task is excluded from ordinary idle waits.
    pub background: bool,
    /// Durable abort mark.
    pub abort_requested: bool,
    /// Durable status without payloads.
    pub state: TaskGraphState,
    /// Conversations this task owns, in ID order.
    pub conversations: Vec<ConversationId>,
}

/// Every live task of the Session (spec §9.5), keyed by decimal ID.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskGraph {
    /// Every live task, keyed by its decimal ID.
    pub tasks: BTreeMap<String, TaskGraphNode>,
}

/// A serialized exact-frame watch of the graph (upstream `TaskGraphWatch`).
pub type TaskGraphWatch = CommittedWatch;

const LIVE_STATUSES: [TaskStatus; 5] = [
    TaskStatus::Pending,
    TaskStatus::Running,
    TaskStatus::Waiting,
    TaskStatus::Sleeping,
    TaskStatus::Completing,
];
const SCAN_PAGE_SIZE: usize = 256;

enum Observer {
    State(Arc<CommittedStateSource>),
    Watch(u64, CommittedWatch),
}

struct Mount {
    value: TaskGraph,
    observers: Vec<Observer>,
}

/// The Harness's task graph mount: built on the Session line by its first
/// observer and dropped with its last.
pub struct TaskGraphView {
    session: Arc<SessionImpl>,
    storage: Arc<dyn Storage>,
    mount: Mutex<Option<Mount>>,
    closed: AtomicBool,
    next_observer: AtomicU64,
}

impl TaskGraphView {
    /// Build the view and subscribe it to the session's commit and close
    /// publications.
    pub fn open(session: Arc<SessionImpl>, storage: Arc<dyn Storage>) -> Arc<Self> {
        let view = Arc::new(Self {
            session: Arc::clone(&session),
            storage,
            mount: Mutex::new(None),
            closed: AtomicBool::new(false),
            next_observer: AtomicU64::new(1),
        });
        let advancing = Arc::clone(&view);
        session.subscribe_commits(Arc::new(move |publication, context| {
            advancing.on_publication(publication, context);
        }));
        let closing = Arc::clone(&view);
        session.subscribe_close(Arc::new(move || {
            closing.close_session();
        }));
        view
    }

    fn on_publication(&self, publication: &CommitPublication, context: &Context) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let Ok(mut guard) = self.mount.lock() else {
            return;
        };
        let Some(mount) = guard.as_mut() else {
            return;
        };
        advance(mount, publication, context);
    }

    fn close_session(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let observers = match self.mount.lock() {
            Ok(mut guard) => guard.take().map(|mount| mount.observers),
            Err(_) => None,
        };
        for observer in observers.into_iter().flatten() {
            match observer {
                Observer::State(source) => source.close_session(),
                Observer::Watch(_, watch) => watch.close_session(),
            }
        }
    }

    /// A disposable read-only Chord state of the graph.
    pub async fn state(
        self: &Arc<Self>,
        context: &Context,
    ) -> SessionResult<AttachedReplicatedState> {
        let this = Arc::clone(self);
        let read_context = context.clone();
        self.session
            .read_on_line(move || {
                let this = Arc::clone(&this);
                Box::pin(async move { this.attach_state(&read_context).await })
            })
            .await
    }

    /// A serialized exact-frame watch of the graph; cancelling `context` stops it.
    pub async fn watch(self: &Arc<Self>, context: &Context) -> SessionResult<TaskGraphWatch> {
        let this = Arc::clone(self);
        let read_context = context.clone();
        self.session
            .read_on_line(move || {
                let this = Arc::clone(&this);
                Box::pin(async move { this.attach_watch(&read_context).await })
            })
            .await
    }

    async fn attach_state(
        self: &Arc<Self>,
        context: &Context,
    ) -> SessionResult<AttachedReplicatedState> {
        if is_cancelled(context) {
            return Err(SessionError::Aborted);
        }
        let value = self.mount_value(context).await?;
        // Close or cancellation may begin while the mount builds.
        if self.closed.load(Ordering::SeqCst) {
            return Err(SessionError::message("Harness is closed"));
        }
        if is_cancelled(context) {
            return Err(SessionError::Aborted);
        }
        let source = Arc::new(CommittedStateSource::new(graph_json(&value)));
        let this = Arc::clone(self);
        let released = Arc::clone(&source);
        source.set_release(Box::new(move || this.detach_state(&released)));
        let state = crate::session::observation::attach_state(&source);
        self.mount_observer(Observer::State(source))?;
        Ok(state)
    }

    async fn attach_watch(self: &Arc<Self>, context: &Context) -> SessionResult<TaskGraphWatch> {
        if is_cancelled(context) {
            return Err(SessionError::Aborted);
        }
        let value = self.mount_value(context).await?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(SessionError::message("Harness is closed"));
        }
        if is_cancelled(context) {
            return Err(SessionError::Aborted);
        }
        let watch = CommittedWatch::new(graph_json(&value));
        let watch_id = self.next_observer.fetch_add(1, Ordering::Relaxed);
        let this = Arc::clone(self);
        watch.set_detach(Box::new(move || this.detach_watch(watch_id)));
        self.mount_observer(Observer::Watch(watch_id, watch.clone()))?;
        if let Some(signal) = context.abort_signal() {
            if signal.is_cancelled() {
                watch.cancel();
                return Err(SessionError::Aborted);
            }
            let cancelling = watch.clone();
            tokio::spawn(async move {
                signal.cancelled().await;
                cancelling.cancel();
            });
        }
        Ok(watch)
    }

    async fn mount_value(self: &Arc<Self>, context: &Context) -> SessionResult<TaskGraph> {
        if let Ok(guard) = self.mount.lock()
            && let Some(mount) = guard.as_ref()
        {
            return Ok(mount.value.clone());
        }
        let value = self.build(context).await?;
        let mut guard = self
            .mount
            .lock()
            .map_err(|_| SessionError::message("Task graph mount lock poisoned"))?;
        match guard.as_ref() {
            Some(mount) => Ok(mount.value.clone()),
            None => {
                *guard = Some(Mount {
                    value: value.clone(),
                    observers: Vec::new(),
                });
                Ok(value)
            }
        }
    }

    fn mount_observer(&self, observer: Observer) -> SessionResult<()> {
        let mut guard = self
            .mount
            .lock()
            .map_err(|_| SessionError::message("Task graph mount lock poisoned"))?;
        match guard.as_mut() {
            Some(mount) => {
                mount.observers.push(observer);
                Ok(())
            }
            None => Err(SessionError::message("Task graph mount disappeared")),
        }
    }

    fn detach_state(&self, source: &Arc<CommittedStateSource>) {
        self.detach(|observer| matches!(observer, Observer::State(existing) if Arc::ptr_eq(existing, source)));
    }

    fn detach_watch(&self, id: u64) {
        self.detach(|observer| matches!(observer, Observer::Watch(existing, _) if *existing == id));
    }

    fn detach(&self, matches: impl Fn(&Observer) -> bool) {
        let Ok(mut guard) = self.mount.lock() else {
            return;
        };
        let Some(mount) = guard.as_mut() else {
            return;
        };
        mount.observers.retain(|observer| !matches(observer));
        if mount.observers.is_empty() {
            *guard = None;
        }
    }

    async fn build(&self, context: &Context) -> SessionResult<TaskGraph> {
        let mut records: Vec<TaskRecord> = Vec::new();
        for status in LIVE_STATUSES {
            let mut cursor = None;
            loop {
                let page = self
                    .storage
                    .scan_tasks(
                        TaskQuery {
                            status: Some(status),
                            ..TaskQuery::default()
                        },
                        SCAN_PAGE_SIZE,
                        cursor,
                        context,
                    )
                    .await?;
                records.extend(page.items);
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
        }
        records.sort_by_key(|record| record.id);
        let mut tasks: BTreeMap<String, TaskGraphNode> = BTreeMap::new();
        for record in &records {
            let owned = self.scan_owned(record.id, context).await?;
            let mut conversations: Vec<ConversationId> =
                owned.iter().map(|conversation| conversation.id).collect();
            conversations.sort();
            tasks.insert(record.id.get().to_string(), node_of(record, conversations));
        }
        Ok(TaskGraph { tasks })
    }

    async fn scan_owned(
        &self,
        task_id: TaskId,
        context: &Context,
    ) -> SessionResult<Vec<ConversationRecord>> {
        let mut conversations = Vec::new();
        let mut cursor = None;
        loop {
            let Page { items, next }: Page<ConversationRecord> = self
                .storage
                .scan_conversations(
                    ConversationQuery {
                        owner_task_id: Some(task_id),
                        ..ConversationQuery::default()
                    },
                    SCAN_PAGE_SIZE,
                    cursor,
                    context,
                )
                .await?;
            conversations.extend(items);
            match next {
                Some(cursor_next) => cursor = Some(cursor_next),
                None => break,
            }
        }
        Ok(conversations)
    }
}

fn is_cancelled(context: &Context) -> bool {
    context
        .abort_signal()
        .is_some_and(|signal| signal.is_cancelled())
}

/// Derive the mount's new revision from one publication and hand it to every
/// observer.
// FORM: upstream applies a hand-built `Op` batch; the port recomputes the graph
// value and emits `diff_json(previous, next)` at the registered tracker shape.
fn advance(mount: &mut Mount, publication: &CommitPublication, context: &Context) {
    let mut next = mount.value.clone();
    for change in &publication.changes {
        if let CommitChange::Table(StorageWrite::Task { value }) = change {
            let key = value.id.get().to_string();
            let previous = next.tasks.get(&key).cloned();
            if value.status() == TaskStatus::Terminal {
                if previous.is_some() {
                    next.tasks.remove(&key);
                }
                continue;
            }
            let conversations = previous
                .as_ref()
                .map(|node| node.conversations.clone())
                .unwrap_or_default();
            let node = node_of(value, conversations);
            if previous.as_ref() == Some(&node) {
                continue;
            }
            next.tasks.insert(key, node);
        }
    }
    // After the tasks, so a conversation created with its owner task in one
    // commit finds the owner's node. Change order within a publication is
    // unspecified, so each owner's list is sorted again.
    for change in &publication.changes {
        if let CommitChange::Table(StorageWrite::Conversation { value }) = change
            && let Some(owner) = &value.owner
        {
            let key = owner.task_id.get().to_string();
            if let Some(node) = next.tasks.get_mut(&key)
                && !node.conversations.contains(&value.id)
            {
                node.conversations.push(value.id);
                node.conversations.sort();
            }
        }
    }
    if next == mount.value {
        return;
    }
    let ops = diff_json(&graph_json(&mount.value), &graph_json(&next));
    mount.value = next;
    let value = graph_json(&mount.value);
    let frame_context = without_abort_signal(context);
    for observer in &mount.observers {
        match observer {
            Observer::State(source) => {
                source.advance(value.clone(), ops.clone(), frame_context.clone())
            }
            Observer::Watch(_, watch) => {
                watch.advance(value.clone(), ops.clone(), frame_context.clone())
            }
        }
    }
}

fn node_of(record: &TaskRecord, conversations: Vec<ConversationId>) -> TaskGraphNode {
    TaskGraphNode {
        id: record.id,
        kind: record.kind.clone(),
        conversation_id: record.conversation_id,
        owner: record.owner,
        background: record.background,
        abort_requested: record.abort_requested,
        state: state_of(record),
        conversations,
    }
}

fn state_of(record: &TaskRecord) -> TaskGraphState {
    match &record.state {
        TaskState::Pending { checkpoint } => TaskGraphState::Pending {
            phase: phase_of(checkpoint),
        },
        TaskState::Running { checkpoint } => TaskGraphState::Running {
            phase: phase_of(checkpoint),
        },
        TaskState::Waiting {
            checkpoint,
            on,
            policy,
        } => TaskGraphState::Waiting {
            phase: phase_of(checkpoint),
            on: on.clone(),
            policy: *policy,
        },
        TaskState::Sleeping { checkpoint, until } => TaskGraphState::Sleeping {
            phase: phase_of(checkpoint),
            until: *until,
        },
        // Terminal records never reach here: they leave the graph.
        TaskState::Completing { outcome } | TaskState::Terminal { outcome } => {
            TaskGraphState::Completing {
                outcome: outcome_status(outcome),
            }
        }
    }
}

fn outcome_status(outcome: &TaskOutcome) -> String {
    match outcome {
        TaskOutcome::Completed { .. } => "completed",
        TaskOutcome::Failed { .. } => "failed",
        TaskOutcome::Aborted { .. } => "aborted",
        TaskOutcome::Orphaned { .. } => "orphaned",
        TaskOutcome::Faulted { .. } => "faulted",
    }
    .to_string()
}

fn phase_of(checkpoint: &JsonValue) -> String {
    checkpoint
        .get("phase")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string()
}

fn graph_json(graph: &TaskGraph) -> JsonValue {
    // Invariant: `TaskGraph` always serializes to a JSON object.
    serde_json::to_value(graph).unwrap_or(JsonValue::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        ConversationOwnership, TaskCreate, TaskOwnership, commit_fn, create_session,
    };
    use crate::storage::MemoryStorage;

    fn context() -> Context {
        crate::chord::context::BACKGROUND_CONTEXT.clone()
    }

    async fn create_task(
        session: &Arc<SessionImpl>,
        conversation: ConversationId,
        kind: &str,
        background: bool,
    ) -> TaskId {
        let kind = kind.to_string();
        session
            .commit(
                commit_fn(move |tx| {
                    let kind = kind.clone();
                    Box::pin(async move {
                        tx.create_task(TaskCreate {
                            kind,
                            version: 1,
                            input: JsonValue::Null,
                            checkpoint: serde_json::json!({ "phase": "prepare" }),
                            ownership: TaskOwnership::Conversation(conversation),
                            conversation_id: Some(conversation),
                            background,
                        })
                        .await
                    })
                }),
                &context(),
            )
            .await
            .expect("create task")
    }

    #[tokio::test]
    async fn follows_task_status_and_publishes_no_noop_revision() {
        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let storage: Arc<dyn Storage> = Arc::clone(session.storage());
        let view = TaskGraphView::open(Arc::clone(&session), storage);
        let conversation = session
            .commit(
                commit_fn(|tx| {
                    Box::pin(async move {
                        Ok(tx
                            .create_conversation(ConversationOwnership::Ownerless)
                            .await?
                            .id)
                    })
                }),
                &context(),
            )
            .await
            .expect("conversation");
        let task = create_task(&session, conversation, "pi.generation", false).await;
        let state = view.state(&context()).await.expect("state");
        let value = state.value();
        assert_eq!(
            value["tasks"][task.get().to_string()]["state"]["status"],
            "pending"
        );
        assert_eq!(
            value["tasks"][task.get().to_string()]["kind"],
            "pi.generation"
        );
        // A no-op commit publishes no revision.
        session
            .commit(
                commit_fn(move |_tx| Box::pin(async move { Ok::<(), SessionError>(()) })),
                &context(),
            )
            .await
            .expect("noop");
        assert_eq!(state.value(), value);
        state.dispose();
    }

    #[tokio::test]
    async fn lists_owned_conversations_in_id_order() {
        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let storage: Arc<dyn Storage> = Arc::clone(session.storage());
        let view = TaskGraphView::open(Arc::clone(&session), storage);
        let context = context();
        let root = session
            .commit(
                commit_fn(|tx| {
                    Box::pin(async move {
                        Ok(tx
                            .create_conversation(ConversationOwnership::Ownerless)
                            .await?
                            .id)
                    })
                }),
                &context,
            )
            .await
            .expect("root");
        let task = create_task(&session, root, "pi.generation", false).await;
        // Two conversations owned by the task; the mount lists them sorted by ID.
        let ids = session
            .commit(
                commit_fn(move |tx| {
                    Box::pin(async move {
                        let first = tx
                            .create_conversation(ConversationOwnership::Task(task))
                            .await?;
                        let second = tx
                            .create_conversation(ConversationOwnership::Task(task))
                            .await?;
                        Ok(vec![first.id, second.id])
                    })
                }),
                &context,
            )
            .await
            .expect("owned");
        let mut sorted = ids.clone();
        sorted.sort();
        let state = view.state(&context).await.expect("state");
        let conversations = state.value()["tasks"][task.get().to_string()]["conversations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let actual: Vec<u64> = conversations.iter().filter_map(JsonValue::as_u64).collect();
        let expected: Vec<u64> = sorted.iter().map(|id| id.get()).collect();
        assert_eq!(actual, expected);
        state.dispose();
    }

    #[tokio::test]
    async fn shares_one_mount_between_observers() {
        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let storage: Arc<dyn Storage> = Arc::clone(session.storage());
        let view = TaskGraphView::open(Arc::clone(&session), storage);
        let first = view.state(&context()).await.expect("first");
        let second = view.state(&context()).await.expect("second");
        assert_eq!(first.value(), second.value());
        first.dispose();
        second.dispose();
        assert!(view.mount.lock().expect("mount").is_none());
    }
}
