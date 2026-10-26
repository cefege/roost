//! Harness composition: conversations, the open/close lifecycle, and the
//! conversation handles (port of `packages/durable/src/harness/harness.ts`
//! @ `abe508e1b`).
//!
//! [`HarnessImpl`] owns a Session kernel and composes the registry, submissions,
//! task graph, and (injected) scheduler. Upstream `Harness extends Session`; the
//! Rust port composes ([`HarnessImpl::session`]) and delegates the document
//! surface, keeping the public method set.
//!
//! The scheduler/generation pipeline composes here: [`HarnessImpl::open`]
//! installs the real [`TaskScheduler`] by default, and
//! [`open_harness_with_scheduler`] lets a caller inject another
//! [`SchedulerService`] (the recording double in tests).

// The design keeps the composition in `harness/harness.rs`; Clippy's
// `module_inception` is a style lint for the resulting `harness::harness` path.
#![allow(clippy::module_inception)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;

use crate::chord::context::{Context, with_abort_signal, without_abort_signal};
use crate::chord::json::{JsonObject, JsonValue};
use crate::chord::state::AttachedReplicatedState;
use crate::documents::DocToken;
use crate::entries::RESET_ENTRY;
use crate::harness::agent::{AGENT_DOC, configure, create_agent, resolve_agent, resolve_settings};
use crate::harness::context::read_context;
use crate::harness::inbox::{INBOX_DOC, QueueModes};
use crate::harness::live::LIVE_DOC;
use crate::harness::provider::PROVIDER_DOC;
use crate::harness::registry::BUILTIN_TASK_NAMES;
use crate::harness::scheduler::{
    InvocationBinding, SchedulerDeps, SchedulerFactory, SchedulerService, TaskScheduler, Wake,
};
use crate::harness::submissions::Submissions;
use crate::harness::task_graph::{TaskGraphView, TaskGraphWatch};
use crate::harness::types::{
    Agent, AgentChange, AgentState, Conversation, ConversationAbortOptions,
    ConversationCreateOptions, ConversationHandle, DocumentReader, EnvTarget, ExecutionEnv,
    Harness, HarnessInspection, HarnessOptions, InputSubmissionDraft, RegistrySnapshot,
    RootOptions, SettledTask, Submission, SubmissionAbortResult, SubmissionDraft, TaskAbort,
};
use crate::harness::usage::{USAGE_DOC, UsageState, add_usage_state};
use crate::ids::{ConversationId, EntryId, ROOT_CONVERSATION_ID, SubmissionId, TaskId};
use crate::session::{
    ConversationCreatedHook, ConversationOwnership, DocOwner, EntryDraft, EntryHead, NowFn,
    SessionError, SessionImpl, SessionResult, Transaction, TransactionScope, commit_fn,
};
use crate::types::{
    ConversationQuery, ConversationRecord, Cursor, EntryQuery, EntryRecord, Page, Storage,
    SubmissionQuery, SubmissionRecord, SubmissionStatusFilter, TaskRecord,
};

const SCAN_PAGE_SIZE: usize = 256;

/// A conversation creation target (upstream `CreateTarget`).
#[derive(Clone, Copy)]
enum CreateTarget {
    Root,
    Independent {
        ownership: ConversationOwnership,
    },
    Fork {
        parent_id: ConversationId,
        at: EntryId,
        ownership: ConversationOwnership,
    },
}

/// What the conveniences apply in the creating commit, after the creation hook.
#[derive(Clone)]
struct CreateOptions {
    agent: Option<AgentChange>,
    init: Option<crate::harness::types::ConversationInit>,
}

/// Document reader over one session (upstream passes the harness itself as the
/// `ContextReader` of `EnvTarget`).
struct SessionDocumentReader {
    session: Arc<SessionImpl>,
}

#[async_trait]
impl DocumentReader for SessionDocumentReader {
    async fn snapshot(
        &self,
        token: &DocToken,
        owner: DocOwner<'_>,
        context: &Context,
    ) -> SessionResult<Option<JsonObject>> {
        self.session.snapshot(token, owner, context).await
    }
}

/// Session kernel extended with conversation handles, submissions, and the task
/// graph.
pub struct HarnessImpl {
    weak: Weak<HarnessImpl>,
    session: Arc<SessionImpl>,
    storage: Arc<dyn Storage>,
    options: HarnessOptions,
    settings: crate::harness::types::Settings,
    report: crate::harness::types::ReportFn,
    tasks: Arc<dyn SchedulerService>,
    submissions: Arc<Submissions>,
    task_graph: Arc<TaskGraphView>,
    closed: AtomicBool,
}

impl HarnessImpl {
    /// The owned Session kernel.
    pub fn session(&self) -> &Arc<SessionImpl> {
        &self.session
    }

    fn shared(&self) -> SessionResult<Arc<HarnessImpl>> {
        self.weak
            .upgrade()
            .ok_or_else(|| SessionError::message("Harness is closed"))
    }

    fn assert_open(&self) -> SessionResult<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(SessionError::message("Harness is closed"));
        }
        Ok(())
    }

    fn conversation_handle(self: &Arc<Self>, id: ConversationId) -> Arc<ConversationImpl> {
        Arc::new(ConversationImpl {
            id,
            harness: Arc::clone(self),
        })
    }

    /// Resolve a conversation's committed `pi.agent` against `snapshot` (or the
    /// current registry) and the resolved settings.
    pub async fn resolve_agent(
        &self,
        id: ConversationId,
        snapshot: Option<Arc<dyn RegistrySnapshot>>,
        context: &Context,
    ) -> SessionResult<Agent> {
        let registry = snapshot.unwrap_or_else(|| self.options.registry.snapshot());
        let value = self
            .session
            .snapshot(&AGENT_DOC, DocOwner::Conversation(id), context)
            .await?;
        let state: Option<AgentState> = match value {
            Some(value) => Some(
                serde_json::from_value(JsonValue::Object(value))
                    .map_err(|error| SessionError::message(error.to_string()))?,
            ),
            None => None,
        };
        Ok(resolve_agent(
            state.as_ref(),
            registry.as_ref(),
            &self.settings,
            &self.report,
        ))
    }

    /// Build a conversation's environment from its current `cwd`; `None` without
    /// an `env` option.
    pub async fn build_env(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> SessionResult<Option<Arc<dyn ExecutionEnv>>> {
        let Some(build) = &self.options.env else {
            return Ok(None);
        };
        let cwd = self
            .session
            .snapshot(&AGENT_DOC, DocOwner::Conversation(id), context)
            .await?
            .and_then(|value| {
                value
                    .get("cwd")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string)
            });
        let read: Arc<dyn DocumentReader> = Arc::new(SessionDocumentReader {
            session: Arc::clone(&self.session),
        });
        Ok(build(
            EnvTarget {
                conversation_id: id,
                cwd,
                read,
            },
            context.clone(),
        )
        .await)
    }

    /// Enable task scheduling (idempotent; errors after close).
    pub fn resume(&self) -> SessionResult<()> {
        self.assert_open()?;
        if self.options.read_only {
            return Err(SessionError::ReadOnlySession("running tasks"));
        }
        self.tasks.resume();
        Ok(())
    }

    /// Current runnable work and earliest future durable sleeping-task deadline.
    pub fn wake(&self) -> SessionResult<Wake> {
        self.assert_open()?;
        if self.options.read_only {
            return Ok(Wake {
                active: false,
                at: None,
            });
        }
        Ok(self.tasks.wake())
    }

    /// Alias emphasizing the next persisted wake target.
    pub fn next_wake(&self) -> SessionResult<Wake> {
        self.wake()
    }

    /// Read one task record.
    pub async fn get_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> SessionResult<Option<TaskRecord>> {
        let session = Arc::clone(&self.session);
        let job_session = Arc::clone(&session);
        let read_context = context.clone();
        session
            .read_on_line(move || {
                Box::pin(async move { Ok(job_session.storage().task(id, &read_context).await?) })
            })
            .await
    }

    /// Live tasks and unsettled submissions.
    pub async fn inspect(&self, context: &Context) -> SessionResult<HarnessInspection> {
        let (scheduling, tasks) = self.tasks.inspect(self.options.registry.snapshot()).await?;
        let session = Arc::clone(&self.session);
        let job_session = Arc::clone(&session);
        let read_context = context.clone();
        let submissions = session
            .read_on_line(move || {
                Box::pin(async move {
                    let queued = scan_submissions(
                        &job_session,
                        SubmissionStatusFilter::Queued,
                        &read_context,
                    )
                    .await?;
                    let placed = scan_submissions(
                        &job_session,
                        SubmissionStatusFilter::Placed,
                        &read_context,
                    )
                    .await?;
                    let mut submissions: Vec<SubmissionRecord> =
                        queued.into_iter().chain(placed).collect();
                    submissions.sort_by_key(|record| record.id());
                    Ok(submissions)
                })
            })
            .await?;
        Ok(HarnessInspection {
            scheduling,
            tasks,
            submissions,
        })
    }

    /// Session total usage.
    pub async fn usage(&self, context: &Context) -> SessionResult<UsageState> {
        let session = Arc::clone(&self.session);
        let job_session = Arc::clone(&session);
        let read_context = context.clone();
        let conversations: Vec<ConversationRecord> = session
            .read_on_line(move || {
                Box::pin(async move { scan_conversations(&job_session, &read_context).await })
            })
            .await?;
        let mut total = UsageState::default();
        for conversation in conversations {
            let value = self
                .session
                .snapshot(&USAGE_DOC, DocOwner::Conversation(conversation.id), context)
                .await?;
            if let Some(value) = value {
                let state: UsageState = serde_json::from_value(JsonValue::Object(value))
                    .map_err(|error| SessionError::message(error.to_string()))?;
                add_usage_state(&mut total, &state);
            }
        }
        Ok(total)
    }

    /// A disposable read-only state of the live task graph.
    pub async fn task_graph(
        self: &Arc<Self>,
        context: &Context,
    ) -> SessionResult<AttachedReplicatedState> {
        self.task_graph.state(context).await
    }

    /// A serialized exact-frame watch of the live task graph.
    pub async fn watch_task_graph(
        self: &Arc<Self>,
        context: &Context,
    ) -> SessionResult<TaskGraphWatch> {
        self.task_graph.watch(context).await
    }

    /// Return the reserved root conversation, creating it when absent.
    pub async fn root(
        self: &Arc<Self>,
        context: &Context,
        options: RootOptions,
    ) -> SessionResult<Arc<ConversationImpl>> {
        self.create(
            CreateTarget::Root,
            CreateOptions {
                agent: options.agent,
                init: options.init,
            },
            context,
        )
        .await
    }

    /// Reacquire a conversation.
    pub async fn conversation(
        self: &Arc<Self>,
        id: ConversationId,
        context: &Context,
    ) -> SessionResult<Option<Arc<ConversationImpl>>> {
        self.assert_open()?;
        let session = Arc::clone(&self.session);
        let job_session = Arc::clone(&session);
        let read_context = context.clone();
        let record = session
            .read_on_line(move || {
                Box::pin(async move {
                    Ok(job_session
                        .storage()
                        .conversation(id, &read_context)
                        .await?)
                })
            })
            .await?;
        Ok(record.map(|record| self.conversation_handle(record.id)))
    }

    /// Create an independent conversation.
    pub async fn create_conversation(
        self: &Arc<Self>,
        options: ConversationCreateOptions,
        context: &Context,
    ) -> SessionResult<Arc<ConversationImpl>> {
        self.assert_open()?;
        self.create(
            CreateTarget::Independent {
                ownership: options.ownership,
            },
            CreateOptions {
                agent: options.agent,
                init: options.init,
            },
            context,
        )
        .await
    }

    /// Commit the abort mark and join an active run invocation.
    pub async fn abort_task(&self, id: TaskId, context: &Context) -> SessionResult<TaskAbort> {
        if self.options.read_only {
            return Err(SessionError::ReadOnlySession("running tasks"));
        }
        self.tasks.resume();
        self.tasks.abort(id, context).await
    }

    /// Resolve with the terminal receipt.
    pub async fn wait_for_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> SessionResult<SettledTask<JsonValue>> {
        self.tasks.resume();
        self.tasks.wait_for_task(id, context).await
    }

    /// Resolve when the ordinary ownership scope of every ownerless conversation
    /// is idle.
    pub async fn wait_for_idle(&self, context: &Context) -> SessionResult<()> {
        if self.options.read_only {
            return Err(SessionError::ReadOnlySession("running tasks"));
        }
        self.tasks.resume();
        self.tasks.wait_for_idle(None, context).await
    }

    /// Reacquire a submission.
    pub async fn submission(
        self: &Arc<Self>,
        id: SubmissionId,
        context: &Context,
    ) -> SessionResult<Option<Arc<dyn Submission>>> {
        self.submissions.get(id, context).await
    }

    /// Abort one submission scoped to a conversation.
    pub async fn abort_submission(
        &self,
        id: SubmissionId,
        context: &Context,
        conversation_id: Option<ConversationId>,
    ) -> SessionResult<SubmissionAbortResult> {
        self.submissions.abort(id, context, conversation_id).await
    }

    /// Close the harness: seal admission, join task invocations, close storage.
    pub async fn close(&self, context: &Context) -> SessionResult<()> {
        self.closed.store(true, Ordering::SeqCst);
        let closing = self.session.close(context);
        let joined = self.tasks.join().await;
        let closed = closing.await;
        joined?;
        closed
    }

    async fn create(
        self: &Arc<Self>,
        target: CreateTarget,
        options: CreateOptions,
        context: &Context,
    ) -> SessionResult<Arc<ConversationImpl>> {
        self.assert_open()?;
        let session = Arc::clone(&self.session);
        let id = session
            .commit_with(
                commit_fn(move |tx| {
                    let target = target;
                    let options = options.clone();
                    Box::pin(async move { create_conversation(tx, target, &options).await })
                }),
                context,
                None,
            )
            .await?;
        Ok(self.conversation_handle(id))
    }
}

async fn create_conversation(
    tx: &mut Transaction,
    target: CreateTarget,
    options: &CreateOptions,
) -> SessionResult<ConversationId> {
    if matches!(target, CreateTarget::Root)
        && tx.conversation(ROOT_CONVERSATION_ID).await?.is_some()
    {
        return Ok(ROOT_CONVERSATION_ID);
    }
    let record = match target {
        CreateTarget::Root => tx.create_root_conversation().await?,
        CreateTarget::Independent { ownership } => tx.create_conversation(ownership).await?,
        CreateTarget::Fork {
            parent_id,
            at,
            ownership,
        } => tx.fork_conversation(parent_id, at, ownership).await?,
    };
    if let Some(agent) = &options.agent {
        configure(tx, record.id, agent).await?;
    }
    if let Some(init) = &options.init {
        init(tx, record.id).await?;
    }
    Ok(record.id)
}

/// The built-in conversation creation hook: empty `pi.live`, `pi.inbox`,
/// `pi.usage`, a fresh `pi.provider`, the conversation's `pi.agent`, then the
/// host hook.
// FORM: the hook closure outlives its `&ConversationRecord` argument, so the
// port clones the record before building the returned future.
fn conversation_created_hook(
    user_hook: Option<ConversationCreatedHook>,
) -> ConversationCreatedHook {
    Arc::new(move |tx, record| {
        let record = record.clone();
        let user_hook = user_hook.clone();
        Box::pin(async move {
            let id = record.id;
            tx.doc(&LIVE_DOC, DocOwner::Conversation(id), None).await?;
            tx.doc(&INBOX_DOC, DocOwner::Conversation(id), None).await?;
            tx.doc(&USAGE_DOC, DocOwner::Conversation(id), None).await?;
            tx.doc(&PROVIDER_DOC, DocOwner::Conversation(id), None)
                .await?;
            create_agent(tx, &record).await?;
            if let Some(user_hook) = user_hook {
                user_hook(tx, &record).await?;
            }
            Ok(())
        })
    })
}

/// Stateless handle for one conversation, bound to the Harness that returned it.
pub struct ConversationImpl {
    id: ConversationId,
    harness: Arc<HarnessImpl>,
}

impl ConversationImpl {
    /// Conversation identity.
    pub fn id(&self) -> ConversationId {
        self.id
    }

    /// Resolve the current agent.
    pub async fn agent(&self, context: &Context) -> SessionResult<Agent> {
        self.harness.resolve_agent(self.id, None, context).await
    }

    /// Apply one agent change in its own commit.
    pub async fn configure(&self, change: AgentChange, context: &Context) -> SessionResult<()> {
        let id = self.id;
        self.harness
            .session
            .commit_with(
                commit_fn(move |tx| {
                    let change = change.clone();
                    Box::pin(async move { configure(tx, id, &change).await })
                }),
                context,
                Some(TransactionScope {
                    conversation_id: Some(id),
                    task_id: None,
                }),
            )
            .await
    }

    /// Admit a submission.
    pub async fn submit(
        &self,
        submission: SubmissionDraft,
        context: &Context,
    ) -> SessionResult<Arc<dyn Submission>> {
        self.harness
            .submissions
            .submit(self.id, submission, context)
            .await
    }

    /// Admit a `pi.reset` write that starts a new context.
    pub async fn reset(&self, handoff: Option<String>, context: &Context) -> SessionResult<()> {
        let mut entry = EntryDraft::new(RESET_ENTRY);
        entry.head = Some(EntryHead::This);
        if let Some(text) = handoff {
            let message = rpi_ai::types::Message::User(rpi_ai::types::UserMessage {
                role: rpi_ai::types::UserRole::User,
                content: rpi_ai::types::UserContent::Text(text),
                timestamp: self.harness.session.now() as i64,
            });
            entry.model = Some(vec![message]);
        }
        let draft = SubmissionDraft::Write(crate::harness::types::WriteSubmissionDraft {
            request_id: None,
            entry,
        });
        self.harness
            .submissions
            .submit(self.id, draft, context)
            .await
            .map(|_| ())
    }

    /// Create a manual compaction task.
    pub async fn compact(
        &self,
        instructions: Option<String>,
        context: &Context,
    ) -> SessionResult<TaskId> {
        self.harness.resume()?;
        let tasks = Arc::clone(&self.harness.tasks);
        let id = self.id;
        self.harness
            .session
            .commit_with(
                commit_fn(move |tx| {
                    let tasks = Arc::clone(&tasks);
                    let instructions = instructions.clone();
                    Box::pin(async move { tasks.compact(tx, id, instructions).await })
                }),
                context,
                Some(TransactionScope {
                    conversation_id: Some(id),
                    task_id: None,
                }),
            )
            .await
    }

    /// Commit one change attributed to this conversation.
    pub async fn commit<T: Send + 'static>(
        &self,
        change: crate::session::CommitFn<T>,
        context: &Context,
    ) -> SessionResult<T> {
        self.harness
            .session
            .commit_with(
                change,
                context,
                Some(TransactionScope {
                    conversation_id: Some(self.id),
                    task_id: None,
                }),
            )
            .await
    }

    /// Committed raw transcript and model context.
    pub async fn context(
        &self,
        context: &Context,
        at: Option<EntryId>,
    ) -> SessionResult<crate::harness::types::ContextView> {
        read_context(
            &self.harness.session,
            &self.harness.storage,
            self.id,
            context,
            at,
        )
        .await
    }

    /// Fork-aware history of this conversation.
    pub async fn entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> SessionResult<Page<EntryRecord>> {
        let mut bounded = query;
        bounded.conversation_id = self.id;
        let session = Arc::clone(&self.harness.session);
        let job_session = Arc::clone(&session);
        let read_context = context.clone();
        session
            .read_on_line(move || {
                Box::pin(async move {
                    Ok(job_session
                        .storage()
                        .scan_entries(bounded, limit, cursor, &read_context)
                        .await?)
                })
            })
            .await
    }

    /// Fork this conversation at `at`.
    pub async fn fork(
        self: &Arc<Self>,
        at: EntryId,
        options: ConversationCreateOptions,
        context: &Context,
    ) -> SessionResult<Arc<ConversationImpl>> {
        self.harness
            .create(
                CreateTarget::Fork {
                    parent_id: self.id,
                    at,
                    ownership: options.ownership,
                },
                CreateOptions {
                    agent: options.agent,
                    init: options.init,
                },
                context,
            )
            .await
    }

    /// Withdraw queued inputs, mark reachable live tasks, and wait for idle.
    pub async fn abort(
        &self,
        context: &Context,
        options: ConversationAbortOptions,
    ) -> SessionResult<()> {
        if self.harness.options.read_only {
            return Err(SessionError::ReadOnlySession("running tasks"));
        }
        self.harness.tasks.resume();
        self.harness
            .tasks
            .abort_conversation(self.id, options.background, context)
            .await
    }

    /// Resolve when the ordinary ownership scope is idle.
    pub async fn wait_for_idle(&self, context: &Context) -> SessionResult<()> {
        if self.harness.options.read_only {
            return Err(SessionError::ReadOnlySession("running tasks"));
        }
        self.harness.tasks.resume();
        self.harness
            .tasks
            .wait_for_idle(Some(self.id), context)
            .await
    }
}

#[async_trait]
impl Conversation for ConversationImpl {
    fn id(&self) -> ConversationId {
        self.id
    }

    async fn agent(&self, context: &Context) -> SessionResult<Agent> {
        ConversationImpl::agent(self, context).await
    }

    async fn configure(&self, change: AgentChange, context: &Context) -> SessionResult<()> {
        ConversationImpl::configure(self, change, context).await
    }

    async fn submit(
        &self,
        submission: SubmissionDraft,
        context: &Context,
    ) -> SessionResult<Arc<dyn Submission>> {
        ConversationImpl::submit(self, submission, context).await
    }

    async fn reset(&self, handoff: Option<String>, context: &Context) -> SessionResult<()> {
        ConversationImpl::reset(self, handoff, context).await
    }

    async fn context(
        &self,
        context: &Context,
        at: Option<EntryId>,
    ) -> SessionResult<crate::harness::types::ContextView> {
        ConversationImpl::context(self, context, at).await
    }

    async fn entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> SessionResult<Page<EntryRecord>> {
        ConversationImpl::entries(self, query, limit, cursor, context).await
    }

    async fn fork(
        &self,
        at: EntryId,
        options: ConversationCreateOptions,
        context: &Context,
    ) -> SessionResult<Arc<dyn Conversation>> {
        let harness = self.harness.shared()?;
        let conversation = harness
            .create(
                CreateTarget::Fork {
                    parent_id: self.id,
                    at,
                    ownership: options.ownership,
                },
                CreateOptions {
                    agent: options.agent,
                    init: options.init,
                },
                context,
            )
            .await?;
        Ok(conversation as Arc<dyn Conversation>)
    }

    async fn abort(
        &self,
        context: &Context,
        options: ConversationAbortOptions,
    ) -> SessionResult<()> {
        ConversationImpl::abort(self, context, options).await
    }

    async fn wait_for_idle(&self, context: &Context) -> SessionResult<()> {
        ConversationImpl::wait_for_idle(self, context).await
    }
}

#[async_trait]
impl Harness for HarnessImpl {
    fn resume(&self) -> SessionResult<()> {
        HarnessImpl::resume(self)
    }
    fn wake(&self) -> SessionResult<Wake> {
        HarnessImpl::wake(self)
    }

    fn next_wake(&self) -> SessionResult<Wake> {
        HarnessImpl::next_wake(self)
    }

    async fn root(
        &self,
        context: &Context,
        options: RootOptions,
    ) -> SessionResult<Arc<dyn Conversation>> {
        let harness = self.shared()?;
        let conversation = HarnessImpl::root(&harness, context, options).await?;
        Ok(conversation as Arc<dyn Conversation>)
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> SessionResult<Option<Arc<dyn Conversation>>> {
        let harness = self.shared()?;
        Ok(HarnessImpl::conversation(&harness, id, context)
            .await?
            .map(|conversation| conversation as Arc<dyn Conversation>))
    }

    async fn create_conversation(
        &self,
        options: ConversationCreateOptions,
        context: &Context,
    ) -> SessionResult<Arc<dyn Conversation>> {
        let harness = self.shared()?;
        let conversation = HarnessImpl::create_conversation(&harness, options, context).await?;
        Ok(conversation as Arc<dyn Conversation>)
    }

    async fn get_task(&self, id: TaskId, context: &Context) -> SessionResult<Option<TaskRecord>> {
        HarnessImpl::get_task(self, id, context).await
    }

    async fn inspect(&self, context: &Context) -> SessionResult<HarnessInspection> {
        HarnessImpl::inspect(self, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> SessionResult<Option<Arc<dyn Submission>>> {
        let harness = self.shared()?;
        harness.submission(id, context).await
    }

    async fn abort_submission(
        &self,
        id: SubmissionId,
        context: &Context,
        conversation_id: Option<ConversationId>,
    ) -> SessionResult<SubmissionAbortResult> {
        HarnessImpl::abort_submission(self, id, context, conversation_id).await
    }

    async fn abort_task(&self, id: TaskId, context: &Context) -> SessionResult<TaskAbort> {
        HarnessImpl::abort_task(self, id, context).await
    }

    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> SessionResult<SettledTask<JsonValue>> {
        HarnessImpl::wait_for_task(self, id, context).await
    }

    async fn wait_for_idle(&self, context: &Context) -> SessionResult<()> {
        HarnessImpl::wait_for_idle(self, context).await
    }

    async fn usage(&self, context: &Context) -> SessionResult<UsageState> {
        HarnessImpl::usage(self, context).await
    }

    async fn close(&self, context: &Context) -> SessionResult<()> {
        HarnessImpl::close(self, context).await
    }
}

/// Invocation-bound handle for tasks and tools. Every operation, and every
/// operation of a submission it returns, first checks the invocation and runs
/// under its signal, so it rejects once the invocation ends.
pub struct BoundConversation {
    id: ConversationId,
    binding: InvocationBinding,
    submissions: Arc<Submissions>,
    tasks: Arc<dyn SchedulerService>,
}

fn bind_context(binding: &InvocationBinding, context: &Context) -> Context {
    with_abort_signal(binding.signal.clone(), context)
}

#[async_trait]
impl ConversationHandle for BoundConversation {
    fn id(&self) -> ConversationId {
        self.id
    }

    async fn submit(
        &self,
        submission: InputSubmissionDraft,
        context: &Context,
    ) -> SessionResult<Arc<dyn Submission>> {
        (self.binding.check)()?;
        let bound = bind_context(&self.binding, context);
        let inner = self
            .submissions
            .submit(self.id, SubmissionDraft::Input(submission), &bound)
            .await?;
        Ok(Arc::new(BoundSubmission {
            id: inner.id(),
            binding: self.binding.clone(),
            inner,
        }))
    }

    async fn abort(
        &self,
        context: &Context,
        options: ConversationAbortOptions,
    ) -> SessionResult<()> {
        (self.binding.check)()?;
        let bound = bind_context(&self.binding, context);
        self.tasks
            .abort_conversation(self.id, options.background, &bound)
            .await
    }

    async fn wait_for_idle(&self, context: &Context) -> SessionResult<()> {
        (self.binding.check)()?;
        let bound = bind_context(&self.binding, context);
        self.tasks.wait_for_idle(Some(self.id), &bound).await
    }
}

/// Build an invocation-bound conversation handle.
pub fn bound_conversation(
    id: ConversationId,
    binding: InvocationBinding,
    submissions: Arc<Submissions>,
    tasks: Arc<dyn SchedulerService>,
) -> Arc<dyn ConversationHandle> {
    Arc::new(BoundConversation {
        id,
        binding,
        submissions,
        tasks,
    })
}

struct BoundSubmission {
    id: SubmissionId,
    binding: InvocationBinding,
    inner: Arc<dyn Submission>,
}

#[async_trait]
impl Submission for BoundSubmission {
    fn id(&self) -> SubmissionId {
        self.id
    }

    async fn status(&self, context: &Context) -> SessionResult<SubmissionRecord> {
        (self.binding.check)()?;
        self.inner
            .status(&bind_context(&self.binding, context))
            .await
    }

    async fn wait(&self, context: &Context) -> SessionResult<SubmissionRecord> {
        (self.binding.check)()?;
        self.inner.wait(&bind_context(&self.binding, context)).await
    }

    async fn abort(
        &self,
        context: &Context,
    ) -> SessionResult<crate::harness::types::SubmissionAbort> {
        (self.binding.check)()?;
        self.inner
            .abort(&bind_context(&self.binding, context))
            .await
    }
}

impl HarnessImpl {
    /// Open a Harness over storage with the real [`TaskScheduler`].
    ///
    /// The registry may keep changing while the Harness runs.
    pub async fn open(
        storage: Arc<dyn Storage>,
        options: HarnessOptions,
        context: &Context,
    ) -> SessionResult<Arc<HarnessImpl>> {
        let factory: SchedulerFactory =
            Arc::new(|deps: SchedulerDeps| TaskScheduler::new(deps) as Arc<dyn SchedulerService>);
        open_harness_with_scheduler(storage, options, factory, context).await
    }
}

/// Open a Harness over storage, injecting the task scheduler (upstream
/// `Harness.open`).
pub async fn open_harness(
    storage: Arc<dyn Storage>,
    options: HarnessOptions,
    context: &Context,
) -> SessionResult<Arc<HarnessImpl>> {
    HarnessImpl::open(storage, options, context).await
}

/// Open a Harness over storage with an explicit scheduler factory. The factory
/// receives the owning session so the scheduler (worker 3a) can subscribe to its
/// publications.
pub async fn open_harness_with_scheduler(
    storage: Arc<dyn Storage>,
    options: HarnessOptions,
    scheduler_factory: SchedulerFactory,
    context: &Context,
) -> SessionResult<Arc<HarnessImpl>> {
    if context
        .abort_signal()
        .is_some_and(|signal| signal.is_cancelled())
    {
        return Err(SessionError::Aborted);
    }
    let snapshot = options.registry.snapshot();
    let missing: Vec<&str> = BUILTIN_TASK_NAMES
        .iter()
        .copied()
        .filter(|name| snapshot.task(name).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(SessionError::message(format!(
            "Registry lacks built-in tasks {}; create it with create_registry()",
            missing.join(", ")
        )));
    }
    let now: NowFn = options
        .now
        .clone()
        .unwrap_or_else(|| Arc::new(crate::session::default_now));
    let settings = resolve_settings(options.settings.as_ref());
    let report = options
        .on_report
        .clone()
        .unwrap_or_else(|| Arc::new(|_| {}));
    let session = SessionImpl::new_with_options(
        Arc::clone(&storage),
        Some(Arc::clone(&now)),
        crate::session::SessionOptions {
            read_only: options.read_only,
            persist_within_ms: settings.progress.persist_within_ms,
            set_timer: options.set_timer.clone(),
        },
    );
    let storage: Arc<dyn Storage> = Arc::clone(session.storage());
    session.set_conversation_created_hook(conversation_created_hook(
        options.conversation_created.clone(),
    ));
    let scheduler = scheduler_factory(SchedulerDeps {
        session: Arc::clone(&session),
        storage: Arc::clone(&storage),
        models: options.models.clone(),
        registry: Arc::clone(&options.registry),
        settings: settings.clone(),
        env: options.env.clone(),
        report: Arc::clone(&report),
        now: Arc::clone(&now),
        context: without_abort_signal(context),
        set_timer: options.set_timer.clone(),
        on_wake: options.on_wake.clone(),
        read_only: options.read_only,
    });
    let resume: Arc<dyn Fn() -> SessionResult<()> + Send + Sync> = {
        let scheduler = Arc::clone(&scheduler);
        let read_only = options.read_only;
        Arc::new(move || {
            if read_only {
                return Err(SessionError::ReadOnlySession("running tasks"));
            }
            scheduler.resume();
            Ok(())
        })
    };
    let queue_modes: Box<dyn Fn() -> QueueModes + Send + Sync> = {
        let settings = settings.clone();
        Box::new(move || QueueModes {
            steering_mode: settings.steering_mode,
            follow_up_mode: settings.follow_up_mode,
        })
    };
    let submissions = Submissions::open(
        Arc::clone(&session),
        Arc::clone(&scheduler),
        now,
        queue_modes,
        resume,
    );
    // Let an invocation-bound conversation handle admit input.
    scheduler.install_submissions(Arc::clone(&submissions));
    let task_graph = TaskGraphView::open(Arc::clone(&session), Arc::clone(&storage));
    let harness = Arc::new_cyclic(|weak| HarnessImpl {
        weak: weak.clone(),
        session,
        storage,
        options,
        settings,
        report,
        tasks: scheduler,
        submissions,
        task_graph,
        closed: AtomicBool::new(false),
    });
    if !harness.options.read_only {
        if let Err(error) = harness.tasks.open(context).await {
            if let Err(close_error) = harness.close(&without_abort_signal(context)).await {
                (harness.report)(close_error.to_string());
            }
            return Err(error);
        }
    }
    Ok(harness)
}

async fn scan_conversations(
    session: &Arc<SessionImpl>,
    context: &Context,
) -> SessionResult<Vec<ConversationRecord>> {
    let mut conversations = Vec::new();
    let mut cursor = None;
    loop {
        let Page { items, next }: Page<ConversationRecord> = session
            .storage()
            .scan_conversations(
                ConversationQuery::default(),
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

async fn scan_submissions(
    session: &Arc<SessionImpl>,
    status: SubmissionStatusFilter,
    context: &Context,
) -> SessionResult<Vec<SubmissionRecord>> {
    let mut submissions = Vec::new();
    let mut cursor = None;
    loop {
        let Page { items, next }: Page<SubmissionRecord> = session
            .storage()
            .scan_submissions(
                SubmissionQuery {
                    status: Some(status),
                    ..SubmissionQuery::default()
                },
                SCAN_PAGE_SIZE,
                cursor,
                context,
            )
            .await?;
        submissions.extend(items);
        match next {
            Some(cursor_next) => cursor = Some(cursor_next),
            None => break,
        }
    }
    Ok(submissions)
}
