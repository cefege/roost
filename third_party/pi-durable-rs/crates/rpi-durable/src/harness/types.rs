//! Harness type surface (port of `packages/durable/src/harness/types.ts`
//! @ `abe508e1b`).
//!
//! This module mirrors the upstream harness type surface as Rust data types and
//! seam traits. Upstream expresses most of it as TypeScript interfaces; the Rust
//! port turns the *behavioural* interfaces (registry, submissions, conversations,
//! harness, tool execution, hooks) into traits and keeps the rest as plain
//! structs/enums. Where the overload-heavy upstream shape cannot map 1:1 the port
//! picks the closest Rust shape and flags the difference with a `// FORM:`
//! comment.
//!
//! Reused upstream types: pi-ai `Message`/`Models`/`Tool`/`Usage`/`AssistantMessage`
//! stay the same types (see `crate::types`).

// Several closure-typed fields (prompt renderers, env factories, wrappers) are
// inherently nested; the seam aliases keep their call sites readable.
#![allow(clippy::type_complexity)]

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use rpi_ai::models::Models;
use rpi_ai::types::{
    AssistantMessage, CacheRetention, Message, ModelThinkingLevel, Tool, ToolCall,
    ToolResultContent, Transport, Usage, UserContent,
};

use crate::chord::context::Context;
use crate::chord::json::{JsonObject, JsonValue};
use crate::documents::DocToken;
use crate::harness::scheduler::Wake;
use crate::ids::{ConversationId, EntryId, SubmissionId, TaskId};
use crate::session::{
    CommitOptions, CommittedWatch, ConversationCreatedHook, ConversationOwnership, DocOwner,
    EntryDraft, SessionError, SessionFuture, SessionResult, TaskOwnership, Transaction,
};
use crate::types::{
    Cursor, EntryQuery, EntryRecord, JoinPolicy, Page, SubmissionRecord, TaskOutcome, TaskRecord,
    TaskState,
};

/// A boxed future returned by the harness seam traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Receives extension failures that do not fail the calling operation
/// (`HarnessOptions.onReport`). Upstream passes an arbitrary thrown value; the
/// Rust port carries its rendered message.
// FORM: upstream `onReport(error: unknown)`; Rust reports a rendered `String`.
pub type ReportFn = Arc<dyn Fn(String) + Send + Sync>;

// ---------------------------------------------------------------------------
// Provider/model references and submissions
// ---------------------------------------------------------------------------

/// Provider and model ID resolved through pi-ai `Models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// Provider ID.
    pub provider: String,
    /// Model ID.
    pub model_id: String,
}

/// Host user input (`UserMessage["content"]`).
pub type UserInput = UserContent;

/// How a submission admitted into a busy conversation is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WhenBusy {
    /// Insert at the next `postTools` boundary.
    Steer,
    /// Queue for the next run's final boundary.
    FollowUp,
    /// Reject with `ConversationBusy` and write nothing.
    Reject,
}

/// Host input submission: user input that may start a run.
#[derive(Debug, Clone)]
pub struct InputSubmissionDraft {
    /// Host deduplication key, scoped to the conversation.
    pub request_id: Option<String>,
    /// User input content.
    pub content: UserInput,
    /// Busy-queue behaviour.
    pub when_busy: Option<WhenBusy>,
}

/// Host write submission: a passive entry write.
#[derive(Debug, Clone)]
pub struct WriteSubmissionDraft {
    /// Host deduplication key, scoped to the conversation.
    pub request_id: Option<String>,
    /// Entry to append.
    pub entry: EntryDraft,
}

/// Host submission (`SubmissionDraft`).
// FORM: upstream intersection type with a `type` discriminant; Rust uses an enum.
#[derive(Debug, Clone)]
pub enum SubmissionDraft {
    /// User input that may start a run.
    Input(InputSubmissionDraft),
    /// Passive entry write.
    Write(WriteSubmissionDraft),
}

/// A terminal submission record (upstream `SettledSubmissionRecord`).
// FORM: upstream restricts `status` to `done|unanswered`; the Rust port aliases
// the record and callers check [`SubmissionRecord::status`].
pub type SettledSubmissionRecord = SubmissionRecord;

/// Outcome of [`Submission::abort`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionAbort {
    /// A queued submission was withdrawn.
    Aborted,
    /// The submission was already placed; it keeps running.
    AlreadyPlaced,
    /// The submission was already terminal.
    Settled,
}

/// Outcome of `Harness::abort_submission` (adds the conversation-scoped miss).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionAbortResult {
    /// A queued submission was withdrawn.
    Aborted,
    /// The submission was already placed.
    AlreadyPlaced,
    /// The submission was already terminal.
    Settled,
    /// Unknown submission or one of another conversation.
    NotFound,
}

/// Awaitable host object for one durably admitted submission.
#[async_trait]
pub trait Submission: Send + Sync {
    /// Durable submission identity.
    fn id(&self) -> SubmissionId;
    /// Current committed record.
    async fn status(&self, context: &Context) -> Result<SubmissionRecord, SessionError>;
    /// Resolve when the submission reaches a terminal status.
    async fn wait(&self, context: &Context) -> Result<SettledSubmissionRecord, SessionError>;
    /// Withdraw a queued submission.
    async fn abort(&self, context: &Context) -> Result<SubmissionAbort, SessionError>;
}

/// A terminal task with its typed result (upstream `SettledTask<R>`).
#[derive(Debug, Clone, PartialEq)]
pub struct SettledTask<R> {
    /// Complete terminal record.
    pub record: TaskRecord,
    /// Typed task result.
    pub result: R,
}

/// Live task record reserved by one invocation (upstream `RunningTask`, `types.ts:137`).
#[derive(Debug, Clone)]
pub struct RunningTask {
    /// Complete committed record; its state is `running` while a run invocation holds it.
    pub record: TaskRecord,
}

impl RunningTask {
    /// Wrap a record whose state is `running`.
    pub fn new(record: TaskRecord) -> Self {
        Self { record }
    }

    /// Task identity.
    pub fn id(&self) -> TaskId {
        self.record.id
    }

    /// Registered definition name.
    pub fn kind(&self) -> &str {
        &self.record.kind
    }

    /// Definition version stored with the record.
    pub fn version(&self) -> u64 {
        self.record.version
    }

    /// Conversation the task belongs to.
    pub fn conversation_id(&self) -> ConversationId {
        self.record.conversation_id
    }

    /// Whether this conversation-owned task is excluded from ordinary idle waits.
    pub fn background(&self) -> bool {
        self.record.background
    }

    /// Durable abort mark.
    pub fn abort_requested(&self) -> bool {
        self.record.abort_requested
    }

    /// Durable checkpoint while the task can run (`pending`/`running`/`waiting`/`sleeping`).
    pub fn checkpoint(&self) -> Option<&JsonValue> {
        match &self.record.state {
            TaskState::Pending { checkpoint }
            | TaskState::Running { checkpoint }
            | TaskState::Waiting { checkpoint, .. }
            | TaskState::Sleeping { checkpoint, .. } => Some(checkpoint),
            TaskState::Completing { .. } | TaskState::Terminal { .. } => None,
        }
    }

    /// The `phase` discriminator of the checkpoint, when present.
    pub fn phase(&self) -> Option<&str> {
        self.checkpoint()?.get("phase").and_then(JsonValue::as_str)
    }
}

/// Next state a task commits for itself (upstream `NextTaskState`, `types.ts:145`).
///
/// A returned [`NextTaskState::Terminal`] is stored as `completing` while the
/// task's ordinary owned work below is live (spec §5.5).
#[derive(Debug, Clone, PartialEq)]
pub enum NextTaskState {
    /// Replace the checkpoint and keep running.
    Running {
        /// Complete durable state from which execution resumes.
        checkpoint: JsonValue,
    },
    /// Park without an invocation until every task in `on` is terminal.
    Waiting {
        /// Complete durable state from which execution resumes.
        checkpoint: JsonValue,
        /// Tasks this task waits on.
        on: Vec<TaskId>,
        /// How the wait reacts to failures.
        policy: JoinPolicy,
    },
    /// Sleep without an invocation until the Harness clock reaches `until`.
    Sleeping {
        /// Complete durable state from which execution resumes.
        checkpoint: JsonValue,
        /// Deadline on the Harness clock, in epoch milliseconds.
        until: f64,
    },
    /// The task's outcome; stored `terminal`, or `completing` while owned work drains.
    Terminal {
        /// Decided outcome.
        outcome: TaskOutcome,
    },
}

impl NextTaskState {
    /// A replacement running checkpoint.
    pub fn running(checkpoint: JsonValue) -> Self {
        Self::Running { checkpoint }
    }

    /// A terminal outcome.
    pub fn terminal(outcome: TaskOutcome) -> Self {
        Self::Terminal { outcome }
    }
}

/// Result of migrating an older stored record at reservation (upstream
/// `TaskDefinition.migrate`).
#[derive(Debug, Clone)]
pub struct Migration {
    /// Migrated task input.
    pub input: JsonValue,
    /// Migrated checkpoint.
    pub checkpoint: JsonValue,
}

/// One transaction-scoped change a phase handler commits through [`TaskRuntime::commit`].
///
/// The closure rereads the task on the Session line; a returned [`NextTaskState`]
/// replaces the task's state in the same commit.
// FORM: upstream passes an inline closure correlated to `Tx`; Rust boxes it so
// the erased runtime stays object-safe.
pub type TaskCommitFn = Arc<
    dyn for<'a> Fn(
            &'a mut Transaction,
            RunningTask,
        ) -> BoxFuture<'a, SessionResult<Option<NextTaskState>>>
        + Send
        + Sync,
>;

/// One atomic conversation write and task-state transition.
pub type TaskWriteCommitFn = Arc<
    dyn for<'a> Fn(
            &'a mut Transaction,
            RunningTask,
            SubmissionId,
        ) -> BoxFuture<'a, SessionResult<Option<NextTaskState>>>
        + Send
        + Sync,
>;

/// One transaction-scoped change a tool execution commits through
/// [`ToolExecutionApi::commit`].
// FORM: upstream `ToolExecutionApi.commit(change)` receives a `Tx`-typed change;
// Rust boxes a `Transaction`-taking closure so the erased API stays object-safe.
pub type ToolCommitFn = Arc<
    dyn for<'a> Fn(&'a mut Transaction) -> BoxFuture<'a, SessionResult<JsonValue>> + Send + Sync,
>;

/// Operations of one task invocation (upstream `TaskRuntime`, `types.ts:170`).
///
/// Every operation rejects after the invocation ends; watches acquired through
/// it stop at invocation end.
#[async_trait]
pub trait TaskRuntime: Send + Sync {
    /// Task identity.
    fn task_id(&self) -> TaskId;
    /// Conversation the task belongs to.
    fn conversation_id(&self) -> ConversationId;
    /// Cancelled when the run is signalled by an abort mark, at close, or when the invocation ends.
    fn signal(&self) -> CancellationToken;
    /// Registry snapshot of the current phase; refreshed at every phase boundary.
    fn registry(&self) -> Arc<dyn RegistrySnapshot>;
    /// pi-ai model catalog.
    fn models(&self) -> &Models;
    /// `HarnessOptions.settings`, resolved.
    fn settings(&self) -> Settings;
    /// The task's conversation's agent, resolved at most once per phase.
    async fn agent(&self, context: &Context) -> SessionResult<Agent>;
    /// Build the task's conversation environment with `HarnessOptions.env`.
    async fn env(&self, context: &Context) -> SessionResult<Option<Arc<dyn ExecutionEnv>>>;
    /// Commit on the Session line after rereading the task.
    async fn commit(&self, change: TaskCommitFn, context: &Context) -> SessionResult<()>;
    /// Commit using explicit document-write persistence options.
    async fn commit_with_options(
        &self,
        change: TaskCommitFn,
        context: &Context,
        options: CommitOptions,
    ) -> SessionResult<()>;
    /// Commit durable sleeping state, then end this invocation so later writes reject.
    async fn sleep_until(
        &self,
        until: f64,
        checkpoint: JsonValue,
        context: &Context,
    ) -> SessionResult<()>;
    /// Arm a task-local timer; it is disarmed when the invocation ends.
    fn set_timer(
        &self,
        callback: Arc<dyn Fn() + Send + Sync>,
        delay_ms: f64,
    ) -> Arc<dyn Fn() + Send + Sync>;
    /// Read a durable memo of this task.
    async fn memo(&self, name: &str, context: &Context) -> SessionResult<Option<JsonValue>>;
    /// Store a memo unless one already exists; return the durable winner.
    async fn memo_set(
        &self,
        name: &str,
        value: JsonValue,
        context: &Context,
    ) -> SessionResult<JsonValue>;
    /// Create a task of `definition` and commit its record on the Session line
    /// (upstream `TaskRuntime.createTask`).
    async fn create_task(
        &self,
        definition: AnyTask,
        input: JsonValue,
        ownership: TaskOwnership,
        background: bool,
        context: &Context,
    ) -> SessionResult<TaskId>;
    /// Atomically admit a conversation write and commit the task transition.
    async fn commit_with_write(
        &self,
        draft: WriteSubmissionDraft,
        change: TaskWriteCommitFn,
        context: &Context,
    ) -> SessionResult<SubmissionId>;
    /// Committed document reads handed to prompt section rendering
    /// (`PromptInput.read`).
    fn document_reader(&self) -> Arc<dyn DocumentReader>;
    /// Stable provider-facing conversation identity, creating it when absent
    /// (`ensureProviderSessionId`).
    async fn provider_session_id(&self, context: &Context) -> SessionResult<String>;
    /// Committed task record.
    async fn get_task(&self, id: TaskId, context: &Context) -> SessionResult<Option<TaskRecord>>;
    /// Resolve with a task's terminal receipt; rejects when the invocation ends.
    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> SessionResult<SettledTask<JsonValue>>;
    /// Live tasks directly owned by this task, in ID order.
    async fn owned_tasks(&self, context: &Context) -> SessionResult<Vec<TaskRecord>>;
    /// Abort and join one directly owned task; other owners are rejected.
    async fn abort_owned(&self, id: TaskId, context: &Context) -> SessionResult<()>;
    /// Outcomes of terminal tasks, in order.
    async fn outcomes(
        &self,
        ids: Vec<TaskId>,
        context: &Context,
    ) -> SessionResult<Vec<TaskOutcome>>;
    /// Invocation-bound handle of an existing conversation.
    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> SessionResult<Option<Arc<dyn ConversationHandle>>>;
    /// Committed entry visible from the task's conversation.
    async fn entry(&self, id: EntryId, context: &Context) -> SessionResult<Option<EntryRecord>>;
    /// Committed raw active transcript and model context.
    async fn context(
        &self,
        conversation_id: ConversationId,
        context: &Context,
        at: Option<EntryId>,
    ) -> SessionResult<ContextView>;
    /// Current committed value of one document, or `None` when absent.
    async fn snapshot(
        &self,
        token: &DocToken,
        owner: DocOwner<'_>,
        context: &Context,
    ) -> SessionResult<Option<JsonObject>>;
    /// Attach a document watch; it stops at invocation end.
    async fn watch_doc(
        &self,
        token: &DocToken,
        owner: DocOwner<'_>,
        context: &Context,
    ) -> SessionResult<Option<CommittedWatch>>;
    /// An owned handle to this runtime for spawned work (the tool task's
    /// throttled progress commits).
    fn handle(&self) -> Arc<dyn TaskRuntime>;
    /// The Harness clock; rejects once the invocation ended.
    fn now(&self) -> SessionResult<f64>;
    /// Forward a non-fatal failure; rejects once the invocation ended.
    fn report(&self, message: String) -> SessionResult<()>;
}

/// Erased executable task definition stored in the registry (upstream `AnyTask`).
///
/// Upstream indexes a phase map by `checkpoint.phase`; the Rust port dispatches
/// on the phase name inside [`TaskDefinition::run_phase`] (registered FORM
/// difference, V17-14 §7.3).
#[async_trait]
pub trait TaskDefinition: Send + Sync {
    /// Registered definition name.
    fn name(&self) -> &str;
    /// Definition version.
    fn version(&self) -> u64;
    /// Initial checkpoint for a new task (`definition.initial`).
    fn initial(&self) -> JsonValue;
    /// Run one phase handler; must commit a changed checkpoint or a terminal outcome.
    ///
    /// The default returns `Ok(())` without a durable change, so a definition
    /// that omits its dispatch faults the task (no durable progress).
    async fn run_phase(
        &self,
        phase: &str,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let _ = (phase, task, runtime, context);
        Ok(())
    }
    /// Run after an abort mark in a fresh invocation; must commit a terminal outcome.
    async fn abort(
        &self,
        task: RunningTask,
        runtime: &dyn TaskRuntime,
        context: &Context,
    ) -> SessionResult<()> {
        let _ = (task, runtime, context);
        Ok(())
    }
    /// Convert a record stored by an older supported version; runs at reservation.
    fn migrate(
        &self,
        input: JsonValue,
        checkpoint: JsonValue,
        from_version: u64,
    ) -> Option<Migration> {
        let _ = (input, checkpoint, from_version);
        None
    }
    /// Whether this definition implements [`TaskDefinition::migrate`].
    ///
    /// Upstream tests `migrate !== undefined`; Rust records it explicitly so
    /// scheduling can take over a newer definition only when it can migrate.
    fn has_migration(&self) -> bool {
        false
    }
}

/// Dyn-compatible task definition (upstream `AnyTask["definition"]`).
pub type AnyTaskDef = dyn TaskDefinition;

/// Erased executable task definition reference.
pub type AnyTask = Arc<AnyTaskDef>;

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Whether the tools of one round run at once or in call order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolExecutionMode {
    /// Run every offered call at once.
    Parallel,
    /// Run one call at a time in call order.
    Sequential,
}

/// How many queued items of one mode a boundary places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    /// Place every queued item.
    #[serde(rename = "all")]
    All,
    /// Place only the first queued item.
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

/// Post-tools controls requested by a tool result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolControl {
    /// Tool names added to the agent after the round.
    pub add_tools: Option<Vec<String>>,
    /// Whether the run terminates after the round.
    pub terminate: Option<bool>,
    /// Handoff text of a terminating run.
    pub handoff: Option<String>,
}

/// Severity of a [`ToolDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSeverity {
    /// Informational remark.
    Info,
    /// Warning.
    Warn,
    /// Error.
    Error,
}

/// Remark about a call for the model and the UI; never part of the tool's data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiagnostic {
    /// Severity.
    pub severity: DiagnosticSeverity,
    /// Human-readable remark.
    pub message: String,
    /// Optional stable code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Result of one tool execution.
// FORM: upstream is generic over `TDetails` and omits absent fields; Rust fixes
// details to `JsonValue` and represents absent fields as `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionResult {
    /// Omitted: the retained `output()` text becomes the content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolResultContent>>,
    /// Structured value checked against `ToolRegistration.structured_output_schema`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<JsonValue>,
    /// Whether the result is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Omitted: the last `details()` value becomes the details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Added after those recorded through `api.diagnostic()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
    /// Spend of the execution itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Post-tools controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ToolControl>,
}

/// What an environment dropped before one output chunk; the V17-15 env
/// domain's `ShellOutputSkip` is the canonical shape.
pub use crate::env::types::ShellOutputSkip as OutputSkip;
/// Output bounds handed to an environment; the V17-15 env domain's
/// `ShellOutputWindow` is the canonical shape.
pub use crate::env::types::ShellOutputWindow as OutputWindow;

/// Which end of a bounded output buffer is retained (`ShellOutputWindow.retain`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRetain {
    /// Keep the head.
    Head,
    /// Keep the tail.
    Tail,
}

/// Failure surface of one tool execution.
pub type ToolExecuteError = Box<dyn std::error::Error + Send + Sync>;

/// Executable tool body (upstream `ToolRegistration.execute`).
#[async_trait]
pub trait ToolExecute: Send + Sync {
    /// Execute one call.
    async fn execute(
        &self,
        args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError>;
}

/// Exponential/linear bounds retained for a tool's running output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputLimits {
    /// Maximum retained bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Maximum retained lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u64>,
    /// Retained end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<OutputRetain>,
}

impl Serialize for OutputRetain {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            OutputRetain::Head => "head",
            OutputRetain::Tail => "tail",
        })
    }
}

impl<'de> Deserialize<'de> for OutputRetain {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "head" => Ok(OutputRetain::Head),
            "tail" => Ok(OutputRetain::Tail),
            other => Err(serde::de::Error::custom(format!(
                "unknown output retain {other}"
            ))),
        }
    }
}

/// Whether an interrupted execution may rerun on recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolReplay {
    /// May rerun on recovery.
    Safe,
    /// Must not rerun (upstream default).
    #[default]
    Unsafe,
}

/// Origin a tool registration permits to invoke it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCaller {
    /// A model-issued call.
    Model,
    /// A tool-issued nested call.
    Tool,
}

/// Executable tool registered in a registry.
///
/// Only the pi-ai [`Tool`] fields enter the transcript; the extra fields drive
/// the harness. Upstream spreads the pi-ai `Tool`; Rust keeps it nested and
/// exposes accessors.
// FORM: upstream `ToolRegistration = Tool & {...}`; Rust nests `tool`.
#[derive(Clone)]
pub struct ToolRegistration {
    /// pi-ai tool declaration entering the transcript.
    pub tool: Tool,
    /// Whether a rerun on recovery is allowed.
    pub replay: ToolReplay,
    /// Default execution mode for this tool.
    pub execution_mode: Option<ToolExecutionMode>,
    /// Repair arguments models commonly get wrong before validation.
    pub prepare_arguments:
        Option<Arc<dyn Fn(&JsonValue) -> Result<JsonValue, String> + Send + Sync>>,
    /// Whether the tool is loaded only after its name is added to `modelTools`.
    pub deferred: bool,
    /// Bounds retained for this tool's running output.
    pub output_limits: Option<OutputLimits>,
    /// Schema for structured output values returned by this tool.
    pub structured_output_schema: Option<JsonValue>,
    /// Call origins permitted to invoke the tool; absent means either origin.
    pub callers: Option<Vec<ToolCaller>>,
    /// Executable body.
    pub execute: Arc<dyn ToolExecute>,
}

impl std::fmt::Debug for ToolRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistration")
            .field("name", &self.tool.name)
            .field("replay", &self.replay)
            .field("execution_mode", &self.execution_mode)
            .finish_non_exhaustive()
    }
}

impl ToolRegistration {
    /// Tool name.
    pub fn name(&self) -> &str {
        &self.tool.name
    }
    /// Whether a call from the given origin may invoke this tool.
    pub fn available_to(&self, caller: ToolCaller) -> bool {
        self.callers
            .as_ref()
            .is_none_or(|callers| callers.contains(&caller))
    }
}

/// One installed tool with its owning extension (upstream `tools()` element).
#[derive(Clone)]
pub struct ToolRef {
    /// Owning extension.
    pub extension: Arc<Extension>,
    /// Installed tool.
    pub tool: ToolRegistration,
}

/// One installed section with its owning extension (upstream `sections()` element).
#[derive(Clone)]
pub struct SectionRef {
    /// Owning extension.
    pub extension: Arc<Extension>,
    /// Installed section.
    pub section: PromptSection,
}

// ---------------------------------------------------------------------------
// Prompt sections, hooks, extensions, registry
// ---------------------------------------------------------------------------

/// Execution environment of one conversation (`ExecutionEnv`); the V17-15 env
/// domain is the canonical surface ([`crate::env::types::ExecutionEnv`]) and
/// V17-14's marker trait is replaced by it.
pub use crate::env::types::ExecutionEnv;

/// Future returned by [`PromptSection::render`].
pub type PromptRenderFuture = Pin<Box<dyn Future<Output = Option<String>> + Send + 'static>>;

/// Render body of a system prompt section.
// FORM: upstream passes borrowed `input`/`context` and may return a promise;
// Rust passes owned clones and always returns a boxed future.
pub type PromptRender = Arc<dyn Fn(PromptInput, Context) -> PromptRenderFuture + Send + Sync>;

/// Reads committed documents without a mutable transaction (`DocumentReader`).
#[async_trait]
pub trait DocumentReader: Send + Sync {
    /// Read the current value of one document, or `None` when absent.
    async fn snapshot(
        &self,
        token: &DocToken,
        owner: DocOwner<'_>,
        context: &Context,
    ) -> Result<Option<JsonObject>, SessionError>;
}

/// Input to system prompt section rendering for one request preparation.
#[derive(Clone)]
pub struct PromptInput {
    /// Conversation being prepared.
    pub conversation_id: ConversationId,
    /// The request's resolution; `agent.tools` are the offered tools.
    pub agent: Agent,
    /// Environment built for this preparation, if any.
    pub env: Option<Arc<dyn ExecutionEnv>>,
    /// Sections already in effect after replaying the active transcript.
    pub shown: BTreeMap<String, String>,
    /// Committed document reads.
    pub read: Arc<dyn DocumentReader>,
}

/// One system prompt section.
#[derive(Clone)]
pub struct PromptSection {
    /// Stable section key.
    pub key: String,
    /// Render body.
    pub render: PromptRender,
    /// Whether the text is wrapped as `<key>\n...\n</key>` (upstream default true).
    pub tag: bool,
}

/// Handlers of one hook registration (upstream `HooksOf<K>`).
// FORM: upstream types handlers by the task's hook generic; Rust erases to the
// known built-in hook interfaces plus a custom box.
#[derive(Clone)]
pub enum HookHandlers {
    /// Built-in generation hooks.
    Generation(Arc<dyn GenerationHooks>),
    /// Built-in tool hooks.
    Tool(Arc<dyn ToolHooks>),
    /// Built-in compaction hooks.
    Compaction(Arc<dyn CompactionHooks>),
    /// Hooks of an extension-defined task.
    Custom(Arc<dyn std::any::Any + Send + Sync>),
}

/// Built by `hook()`; matches tasks by name.
#[derive(Clone)]
pub struct HookRegistration {
    /// Task name the handlers target.
    pub task: String,
    /// Handler bundle.
    pub handlers: HookHandlers,
}

/// Built by `wrapTool()`/`wrapSection()`; targets a tool name or a section key.
#[derive(Clone)]
pub enum Wrap {
    /// Wrap the tool with `tool`'s name.
    Tool {
        /// Target tool name.
        tool: String,
        /// Pure wrapper.
        wrap: Arc<dyn Fn(ToolRegistration) -> ToolRegistration + Send + Sync>,
    },
    /// Wrap the section with `key`.
    Section {
        /// Target section key.
        key: String,
        /// Pure wrapper.
        wrap: Arc<dyn Fn(PromptSection) -> PromptSection + Send + Sync>,
    },
}

/// Named bundle of code installed in a registry and selected by name.
#[derive(Clone, Default)]
pub struct Extension {
    /// Extension name.
    pub name: String,
    /// Offered tools.
    pub tools: Vec<ToolRegistration>,
    /// Offered prompt sections.
    pub sections: Vec<PromptSection>,
    /// Hook registrations.
    pub hooks: Vec<HookRegistration>,
    /// Wrappers applied where this extension is selected.
    pub wraps: Vec<Wrap>,
    /// Task definitions resolved by name for every task.
    pub tasks: Vec<AnyTask>,
}

impl std::fmt::Debug for Extension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extension")
            .field("name", &self.name)
            .field("tools", &self.tools)
            .field("sections", &self.sections.len())
            .field("hooks", &self.hooks.len())
            .field("wraps", &self.wraps.len())
            .field("tasks", &self.tasks.len())
            .finish()
    }
}

/// Immutable view of one published registry state.
// FORM: upstream returns readonly arrays; Rust returns owned vectors.
pub trait RegistrySnapshot: Send + Sync {
    /// Installed extensions in install order.
    fn installed(&self) -> Vec<Arc<Extension>>;
    /// The installed extension with `name`.
    fn extension(&self, name: &str) -> Option<Arc<Extension>>;
    /// Every installed tool with its extension, in install order.
    fn tools(&self) -> Vec<ToolRef>;
    /// Every installed section with its extension, in install order.
    fn sections(&self) -> Vec<SectionRef>;
    /// Built-in and installed task definitions.
    fn tasks(&self) -> Vec<AnyTask>;
    /// The task definition with `name`.
    fn task(&self, name: &str) -> Option<AnyTask>;
}

/// Wakes the scheduler after a registry publication.
pub type RegistryListener = Arc<dyn Fn() + Send + Sync>;

/// Read side of a registry consumed by a Harness.
pub trait RegistryReader: Send + Sync {
    /// Immutable view of the whole current registry.
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot>;
    /// Run `listener` synchronously after every publication.
    fn subscribe(&self, listener: RegistryListener) -> crate::session::Subscription;
}

/// Application-owned registry of extensions.
pub trait Registry: RegistryReader {
    /// Install `extension`, replacing the installed one with its name in place.
    fn install(&self, extension: Extension) -> Result<(), RegistryError>;
    /// Remove the installed extension with `extension.name`.
    fn uninstall(&self, extension: &Extension);
}

/// Failure modes of registry installation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Two extensions (or an extension and a built-in) claim one task name.
    #[error("Task {name} of extension {extension} is already installed")]
    DuplicateTaskName {
        /// Claimed task name.
        name: String,
        /// Claiming extension.
        extension: String,
    },
    /// One extension declares two tools with the same name.
    #[error("Extension {extension} has two tools named {name}")]
    DuplicateToolName {
        /// Extension name.
        extension: String,
        /// Duplicated tool name.
        name: String,
    },
    /// A section key does not match the allowed shape.
    #[error("Section key {key:?} must match {pattern}")]
    InvalidSectionKey {
        /// Offending key.
        key: String,
        /// Required pattern.
        pattern: &'static str,
    },
    /// A section uses the reserved `instructions` key.
    #[error("Section key {key} is reserved for the agent's instructions")]
    ReservedSectionKey {
        /// Offending key.
        key: String,
    },
    /// One extension declares two sections with the same key.
    #[error("Extension {extension} has two sections with key {key}")]
    DuplicateSectionKey {
        /// Extension name.
        extension: String,
        /// Duplicated key.
        key: String,
    },
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// Stored extension selection of one conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionsSelection {
    /// Select exactly these extensions, in order.
    List(Vec<String>),
    /// Edit the host default selection.
    Edit {
        /// Names appended to the default selection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        add: Option<Vec<String>>,
        /// Names removed from the default selection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove: Option<Vec<String>>,
    },
}

/// Stored tool filter of one conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolsSelection {
    /// Offer exactly these tools, in order.
    List(Vec<String>),
    /// Remove these tools.
    Remove {
        /// Removed tool names.
        remove: Vec<String>,
    },
}

/// Stored model-offer filter and deferred-tool load order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelToolsSelection {
    /// Offer exactly these model-callable tools, in order.
    List(Vec<String>),
    /// Keep non-deferred tools except `remove`, then load deferred names in `add` order.
    Edit {
        /// Names removed from the default offer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove: Option<Vec<String>>,
        /// Deferred tool names loaded into the offer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        add: Option<Vec<String>>,
    },
}

/// Stored choices of one conversation; names, not objects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentState {
    /// Model reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Thinking level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Extension selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ExtensionsSelection>,
    /// Tools enabled for calls and model offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsSelection>,
    /// Model-offered subset and deferred-tool load order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_tools: Option<ModelToolsSelection>,
    /// Free-form instructions rendered as the section `instructions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Environment working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Edit of a stored extension selection.
#[derive(Clone, Default)]
pub struct ExtensionsEdit {
    /// Extensions appended to the stored selection.
    pub add: Option<Vec<Arc<Extension>>>,
    /// Extensions removed from the stored selection.
    pub remove: Option<Vec<Arc<Extension>>>,
}

/// Extension change carried by [`AgentChange`].
#[derive(Clone)]
pub enum ExtensionsChange {
    /// Replace the stored selection with exactly these extensions.
    List(Vec<Arc<Extension>>),
    /// Edit the stored selection.
    Edit(ExtensionsEdit),
}

/// Tool change carried by [`AgentChange`].
#[derive(Clone)]
pub enum ToolsChange {
    /// Replace the stored filter with exactly these tools, in order.
    List(Vec<ToolRegistration>),
    /// Remove these tools.
    Remove(Vec<ToolRegistration>),
}

/// Change to the model-offered tool filter.
#[derive(Clone)]
pub enum ModelToolsChange {
    /// Offer exactly these tools, in order.
    List(Vec<ToolRegistration>),
    /// Edit the default offer and deferred load order.
    Edit(ModelToolsEdit),
}

/// Edit of model-offered tools by name.
#[derive(Clone, Default)]
pub struct ModelToolsEdit {
    /// Load these deferred tools in the given order.
    pub add: Option<Vec<ToolRegistration>>,
    /// Remove these tools from the model offer.
    pub remove: Option<Vec<ToolRegistration>>,
}

/// A change to `pi.agent`: a given field replaces the stored one, `null` clears
/// it, `undefined` changes nothing.
// FORM: Rust models upstream `field?: T | null` as `Option<Option<T>>`
// (outer `None` = absent, inner `None` = clear).
#[derive(Clone, Default)]
pub struct AgentChange {
    /// Model change.
    pub model: Option<Option<ModelRef>>,
    /// Thinking-level change.
    pub thinking_level: Option<Option<ModelThinkingLevel>>,
    /// Extension-selection change.
    pub extensions: Option<Option<ExtensionsChange>>,
    /// Tool-filter change.
    pub tools: Option<Option<ToolsChange>>,
    /// Model-offered tool filter and deferred load order.
    pub model_tools: Option<Option<ModelToolsChange>>,
    /// Instructions change.
    pub instructions: Option<Option<String>>,
    /// Working-directory change.
    pub cwd: Option<Option<String>>,
}

/// A conversation's agent resolved against a registry snapshot and the settings.
#[derive(Clone)]
pub struct Agent {
    /// Resolved model.
    pub model: Option<ModelRef>,
    /// Resolved thinking level.
    pub thinking_level: ModelThinkingLevel,
    /// Selected extensions.
    pub extensions: Vec<Arc<Extension>>,
    /// Tools a request offers to the model, in order.
    pub tools: Vec<ToolRegistration>,
    /// Tools callable by other tools.
    pub callable: Vec<ToolRegistration>,
    /// Model-callable deferred tools not yet offered.
    pub deferred: Vec<ToolRegistration>,
    /// Extension sections, then `instructions` when set.
    pub sections: Vec<PromptSection>,
    /// Free-form instructions.
    pub instructions: Option<String>,
    /// Environment working directory.
    pub cwd: Option<String>,
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Curated pi-ai request options; absent fields use pi-ai defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationStreamOptions {
    /// Transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    /// Request timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Provider/SDK retries inside one request attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Maximum provider retry delay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    /// Extra request headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Request metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonObject>,
    /// Prompt cache retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    /// Deferred-response opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredOption>,
}

/// Deferred-response opt-in (`deferred?: boolean | { window? }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeferredOption {
    /// Plain enable/disable flag.
    Enabled(bool),
    /// Windowed enable.
    Window {
        /// Requested window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<DeferredWindow>,
    },
}

/// Provider-side deferred retention window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredWindow {
    /// Fifteen minutes.
    #[serde(rename = "15m")]
    Min15,
    /// One hour.
    #[serde(rename = "1h")]
    Hour1,
    /// Twenty-four hours.
    #[serde(rename = "24h")]
    Hour24,
}

/// Durable generation attempt retries (JSON shape of pi-ai `RetryPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRetryPolicy {
    /// Whether retries are enabled.
    pub enabled: bool,
    /// Maximum attempts after the first.
    pub max_retries: u32,
    /// Base delay in milliseconds.
    pub base_delay_ms: u64,
    /// Maximum delay the agent grows to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_delay_ms: Option<u64>,
}

/// Partial retry policy (upstream `Partial<ConversationRetryPolicy>`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConversationRetryPolicyPatch {
    /// Whether retries are enabled.
    pub enabled: Option<bool>,
    /// Maximum attempts after the first.
    pub max_retries: Option<u32>,
    /// Base delay in milliseconds.
    pub base_delay_ms: Option<u64>,
    /// Maximum delay.
    pub max_agent_delay_ms: Option<u64>,
}

/// Automatic compaction thresholds (spec §8.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPolicy {
    /// Threshold and overflow compaction.
    pub enabled: bool,
    /// Room kept free for the answer.
    pub reserve_tokens: u64,
    /// Approximate recent context kept verbatim.
    pub keep_recent_tokens: u64,
    /// Background compaction threshold offset; `0` disables it.
    pub background_tokens: u64,
}

/// Partial compaction policy (upstream `Partial<CompactionPolicy>`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionPolicyPatch {
    /// Whether compaction is enabled.
    pub enabled: Option<bool>,
    /// Reserved answer room.
    pub reserve_tokens: Option<u64>,
    /// Verbatim recent context.
    pub keep_recent_tokens: Option<u64>,
    /// Background threshold offset.
    pub background_tokens: Option<u64>,
}

/// Progress update pacing and the maximum in-memory persistence window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPolicy {
    /// Minimum pause between answer commits.
    pub partial_interval_ms: u64,
    /// Minimum pause between running tool-output commits.
    pub output_interval_ms: u64,
    /// Maximum time deferred document changes stay in memory; infinity waits for a dependent write or close.
    pub persist_within_ms: f64,
}

/// Partial progress policy.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProgressPolicyPatch {
    /// Answer commit interval.
    pub partial_interval_ms: Option<u64>,
    /// Tool-output commit interval.
    pub output_interval_ms: Option<u64>,
    /// Persistence window; `None` retains the resolved default.
    pub persist_within_ms: Option<f64>,
}

/// Why a compaction runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactionReason {
    /// `compact()`.
    Manual,
    /// A threshold in generation preparation.
    Threshold,
    /// A context overflow.
    Overflow,
}

/// Result of a compaction summary placement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    /// Blocking compaction summary entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<EntryId>,
    /// Conversation-owned compaction summary submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<SubmissionId>,
}

/// Harness-wide run policy as supplied by the host.
#[derive(Clone, Default)]
pub struct HarnessSettings {
    /// Default extension selection; absent: every installed extension.
    pub extensions: Option<Vec<Arc<Extension>>>,
    /// Request options.
    pub stream: Option<ConversationStreamOptions>,
    /// Retry policy overrides.
    pub retry: Option<ConversationRetryPolicyPatch>,
    /// Compaction policy overrides.
    pub compaction: Option<CompactionPolicyPatch>,
    /// Progress policy overrides.
    pub progress: Option<ProgressPolicyPatch>,
    /// Default tool execution mode.
    pub tool_execution: Option<ToolExecutionMode>,
    /// Steering queue mode.
    pub steering_mode: Option<QueueMode>,
    /// Follow-up queue mode.
    pub follow_up_mode: Option<QueueMode>,
    /// Idle context retention in milliseconds.
    pub context_retention_ms: Option<u64>,
}

/// Resolved settings: every field over its built-in default, objects merged.
#[derive(Clone)]
pub struct Settings {
    /// Default extension selection; absent: every installed extension.
    pub extensions: Option<Vec<Arc<Extension>>>,
    /// Request options.
    pub stream: ConversationStreamOptions,
    /// Retry policy.
    pub retry: ConversationRetryPolicy,
    /// Compaction policy.
    pub compaction: CompactionPolicy,
    /// Progress policy.
    pub progress: ProgressPolicy,
    /// Default tool execution mode.
    pub tool_execution: ToolExecutionMode,
    /// Steering queue mode.
    pub steering_mode: QueueMode,
    /// Follow-up queue mode.
    pub follow_up_mode: QueueMode,
    /// Idle context retention in milliseconds.
    pub context_retention_ms: u64,
}

/// What `HarnessOptions.env` builds an environment for.
#[derive(Clone)]
pub struct EnvTarget {
    /// Conversation being built.
    pub conversation_id: ConversationId,
    /// The conversation's agent `cwd`.
    pub cwd: Option<String>,
    /// Committed document reads.
    pub read: Arc<dyn DocumentReader>,
}

/// Environment factory (`HarnessOptions.env`).
pub type EnvFactory = Arc<
    dyn Fn(EnvTarget, Context) -> BoxFuture<'static, Option<Arc<dyn ExecutionEnv>>> + Send + Sync,
>;

/// Harness construction options.
pub struct HarnessOptions {
    /// pi-ai model access used by generation.
    pub models: Models,
    /// Application-owned registry reader.
    pub registry: Arc<dyn RegistryReader>,
    /// Host settings overrides.
    pub settings: Option<HarnessSettings>,
    /// Environment factory.
    pub env: Option<EnvFactory>,
    /// Runs in every commit that creates or forks a conversation.
    pub conversation_created: Option<ConversationCreatedHook>,
    /// Wall clock.
    pub now: Option<crate::session::NowFn>,
    /// Receives extension failures.
    pub on_report: Option<ReportFn>,
    /// Open without recovery, scheduling, or any write admission.
    pub read_only: bool,
    /// Arms harness and session background timers.
    pub set_timer: Option<crate::harness::scheduler::SetTimer>,
    /// Host-owned wakeup callback for sleeping tasks.
    pub on_wake: Option<Arc<dyn Fn(crate::harness::scheduler::Wake) + Send + Sync>>,
}

// ---------------------------------------------------------------------------
// Inspection and context views
// ---------------------------------------------------------------------------

/// Scheduling state of one harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduling {
    /// Scheduling is not enabled.
    Paused,
    /// Scheduling is enabled.
    Running,
    /// The harness is closing.
    Closing,
}

/// Why a live task is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedReason {
    /// No registered definition matches the task kind.
    MissingTask,
    /// The record is older than the registered definition and has no migration.
    TaskTooOld,
    /// The registered definition's migration failed.
    MigrationFailed,
}

/// What the scheduler would do with one live task.
#[derive(Debug, Clone)]
pub enum TaskInspectionState {
    /// An invocation is active.
    Running,
    /// The next pass reserves it.
    Ready {
        /// Whether its definition is newer and has a migration.
        migrates: bool,
    },
    /// Waits for these live tasks.
    Waiting {
        /// Waited task IDs.
        on: Vec<TaskId>,
    },
    /// Sleeps until the Harness clock reaches this deadline.
    Sleeping {
        /// Deadline on the Harness clock, in epoch milliseconds.
        until: f64,
    },
    /// Outcome held until its ordinary owned work drains.
    Completing,
    /// No registered definition can take it.
    Blocked {
        /// Why.
        reason: BlockedReason,
        /// Migration failure text, when any.
        error: Option<String>,
    },
}

/// Live task and what the scheduler would do with it.
#[derive(Debug, Clone)]
pub struct TaskInspection {
    /// Live task record.
    pub record: TaskRecord,
    /// Inspection state.
    pub state: TaskInspectionState,
}

/// Point-in-time view of live work.
#[derive(Debug, Clone)]
pub struct HarnessInspection {
    /// Scheduling state.
    pub scheduling: Scheduling,
    /// Live tasks.
    pub tasks: Vec<TaskInspection>,
    /// Queued and placed submissions, in ID order.
    pub submissions: Vec<SubmissionRecord>,
}

/// Raw active transcript and derived model context.
#[derive(Debug, Clone)]
pub struct ContextView {
    /// Newest applicable head marker, if any.
    pub head: Option<EntryRecord>,
    /// Raw active entries.
    pub entries: Vec<EntryRecord>,
    /// Per entry of `entries`, its model messages.
    pub contributions: Vec<Vec<Message>>,
    /// Model context for the next provider request.
    pub messages: Vec<Message>,
}

// ---------------------------------------------------------------------------
// Conversations and harness
// ---------------------------------------------------------------------------

/// Inside-creation-commit initializer (upstream `ConversationInit`).
pub type ConversationInit =
    Arc<dyn for<'a> Fn(&'a mut Transaction, ConversationId) -> SessionFuture<'a, ()> + Send + Sync>;

/// Options of `Conversation.abort()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConversationAbortOptions {
    /// Cross background boundaries.
    pub background: bool,
}

/// Options of `Harness.create_conversation()`.
#[derive(Clone)]
pub struct ConversationCreateOptions {
    /// Ownership of the new conversation.
    pub ownership: ConversationOwnership,
    /// Applied in the creating commit.
    pub agent: Option<AgentChange>,
    /// Runs in the creating commit.
    pub init: Option<ConversationInit>,
}

/// Options of `Harness.root()`.
#[derive(Clone, Default)]
pub struct RootOptions {
    /// Applied when the root conversation is created.
    pub agent: Option<AgentChange>,
    /// Runs when the root conversation is created.
    pub init: Option<ConversationInit>,
}

/// Outcome of `Harness.abort_task()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAbort {
    /// The abort mark was committed.
    Marked,
    /// The task is already terminal.
    Terminal,
}

/// Invocation-bound conversation operations for tasks and tools.
#[async_trait]
pub trait ConversationHandle: Send + Sync {
    /// Conversation identity.
    fn id(&self) -> ConversationId;
    /// Admit user input.
    async fn submit(
        &self,
        submission: InputSubmissionDraft,
        context: &Context,
    ) -> Result<Arc<dyn Submission>, SessionError>;
    /// Withdraw queued inputs and abort the ordinary ownership scope.
    async fn abort(
        &self,
        context: &Context,
        options: ConversationAbortOptions,
    ) -> Result<(), SessionError>;
    /// Resolve when the ordinary ownership scope has no live non-background task.
    async fn wait_for_idle(&self, context: &Context) -> Result<(), SessionError>;
}

/// Stateless handle for one conversation, bound to the Harness that returned it.
// The upstream `commit<T>` / `viewState` / `watch` / `taskGraph` methods are the
// scheduler/view workers' surface; this seam carries the core set.
#[async_trait]
pub trait Conversation: Send + Sync {
    /// Conversation identity.
    fn id(&self) -> ConversationId;
    /// Resolve the current agent.
    async fn agent(&self, context: &Context) -> Result<Agent, SessionError>;
    /// Apply one agent change in its own commit.
    async fn configure(&self, change: AgentChange, context: &Context) -> Result<(), SessionError>;
    /// Admit user input or a passive entry write.
    async fn submit(
        &self,
        submission: SubmissionDraft,
        context: &Context,
    ) -> Result<Arc<dyn Submission>, SessionError>;
    /// Admit a `pi.reset` write that starts a new context.
    async fn reset(&self, handoff: Option<String>, context: &Context) -> Result<(), SessionError>;
    /// Committed raw transcript and model context.
    async fn context(
        &self,
        context: &Context,
        at: Option<EntryId>,
    ) -> Result<ContextView, SessionError>;
    /// Fork-aware history of this conversation.
    async fn entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &Context,
    ) -> Result<Page<EntryRecord>, SessionError>;
    /// Fork this conversation at `at`.
    async fn fork(
        &self,
        at: EntryId,
        options: ConversationCreateOptions,
        context: &Context,
    ) -> Result<Arc<dyn Conversation>, SessionError>;
    /// Withdraw queued inputs, mark live tasks, and wait for idle.
    async fn abort(
        &self,
        context: &Context,
        options: ConversationAbortOptions,
    ) -> Result<(), SessionError>;
    /// Resolve when the ordinary ownership scope is idle.
    async fn wait_for_idle(&self, context: &Context) -> Result<(), SessionError>;
}

/// Durable agent harness over one Session.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Enable task scheduling (idempotent; errors after close).
    fn resume(&self) -> Result<(), SessionError>;
    /// Current runnable work and earliest future durable sleeping-task deadline.
    fn wake(&self) -> Result<Wake, SessionError>;
    /// Alias emphasizing the next persisted wake target.
    fn next_wake(&self) -> Result<Wake, SessionError>;
    /// Return the reserved root conversation, creating it when absent.
    async fn root(
        &self,
        context: &Context,
        options: RootOptions,
    ) -> Result<Arc<dyn Conversation>, SessionError>;
    /// Reacquire a conversation.
    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> Result<Option<Arc<dyn Conversation>>, SessionError>;
    /// Create a conversation.
    async fn create_conversation(
        &self,
        options: ConversationCreateOptions,
        context: &Context,
    ) -> Result<Arc<dyn Conversation>, SessionError>;
    /// Read one task record.
    async fn get_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> Result<Option<TaskRecord>, SessionError>;
    /// Live tasks and unsettled submissions.
    async fn inspect(&self, context: &Context) -> Result<HarnessInspection, SessionError>;
    /// Reacquire a submission.
    async fn submission(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> Result<Option<Arc<dyn Submission>>, SessionError>;
    /// Abort one submission scoped to a conversation.
    async fn abort_submission(
        &self,
        id: SubmissionId,
        context: &Context,
        conversation_id: Option<ConversationId>,
    ) -> Result<SubmissionAbortResult, SessionError>;
    /// Commit the abort mark, signal, and join an active run invocation.
    async fn abort_task(&self, id: TaskId, context: &Context) -> Result<TaskAbort, SessionError>;
    /// Resolve with the terminal receipt.
    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> Result<SettledTask<JsonValue>, SessionError>;
    /// Resolve when the ordinary ownership scope of every ownerless conversation is idle.
    async fn wait_for_idle(&self, context: &Context) -> Result<(), SessionError>;
    /// Session total usage.
    async fn usage(
        &self,
        context: &Context,
    ) -> Result<crate::harness::usage::UsageState, SessionError>;
    /// Close the harness.
    async fn close(&self, context: &Context) -> Result<(), SessionError>;
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// What a hook may use: committed reads and the asking task's memos.
// FORM: upstream overloads `memo(name[, candidate])`; Rust splits them.
#[async_trait]
pub trait HookApi: Send + Sync {
    /// Asking task.
    fn task_id(&self) -> TaskId;
    /// Asking conversation.
    fn conversation_id(&self) -> ConversationId;
    /// pi-ai model catalog.
    fn models(&self) -> &Models;
    /// Read one memo.
    async fn memo(&self, name: &str, context: &Context) -> Result<Option<JsonValue>, SessionError>;
    /// Write one memo when absent, returning the retained value.
    async fn memo_set(
        &self,
        name: &str,
        value: JsonValue,
        context: &Context,
    ) -> Result<JsonValue, SessionError>;
}

/// [`HookApi`] backed by one task invocation.
#[derive(Clone, Copy)]
pub(crate) struct RuntimeHookApi<'a> {
    /// Runtime of the invocation running the hook.
    pub runtime: &'a dyn TaskRuntime,
}

#[async_trait]
impl HookApi for RuntimeHookApi<'_> {
    fn task_id(&self) -> TaskId {
        self.runtime.task_id()
    }

    fn conversation_id(&self) -> ConversationId {
        self.runtime.conversation_id()
    }

    fn models(&self) -> &Models {
        self.runtime.models()
    }

    async fn memo(&self, name: &str, context: &Context) -> Result<Option<JsonValue>, SessionError> {
        self.runtime.memo(name, context).await
    }

    async fn memo_set(
        &self,
        name: &str,
        value: JsonValue,
        context: &Context,
    ) -> Result<JsonValue, SessionError> {
        self.runtime.memo_set(name, value, context).await
    }
}

/// `beforeRequest` replacement payload.
#[derive(Debug, Clone)]
pub struct RequestMessages {
    /// Messages used for that request only.
    pub messages: Vec<Message>,
}

/// `onYield` continuation payload.
#[derive(Debug, Clone)]
pub struct ContinueInput {
    /// User input continuing the run.
    pub continue_input: UserInput,
}

/// `beforeTool` decision.
#[derive(Debug, Clone, Default)]
pub struct BeforeToolResult {
    /// Replacement arguments.
    pub arguments: Option<JsonObject>,
    /// Block reason.
    pub block: Option<String>,
}

/// Hooks of the built-in generation task.
#[async_trait]
pub trait GenerationHooks: Send + Sync {
    /// Apply host policy before the next request's agent choices are pinned.
    async fn prepare_agent(
        &self,
        _agent: &Agent,
        _api: &dyn HookApi,
        _context: &Context,
    ) -> SessionResult<Option<Agent>> {
        Ok(None)
    }
    /// A checked admission gate for the current request attempt.
    ///
    /// Unlike ordinary extension hooks, failures here reject this request and
    /// propagate to the generation task. The optional replacement is scoped to
    /// this invocation.
    async fn before_request_gate(
        &self,
        _request: &RequestMessages,
        _api: &dyn HookApi,
        _context: &Context,
    ) -> SessionResult<Option<RequestMessages>> {
        Ok(None)
    }
    /// Before every request attempt; ordinary hook failures are report-and-continue.
    async fn before_request(
        &self,
        request: RequestMessages,
        api: &dyn HookApi,
        context: &Context,
    ) -> Option<RequestMessages>;
    /// Every terminal provider message, before classification.
    async fn after_response(
        &self,
        message: &AssistantMessage,
        api: &dyn HookApi,
        context: &Context,
    );
    /// A final answer; the first `continue` appends a user message and continues.
    async fn on_yield(
        &self,
        answer: &AssistantMessage,
        api: &dyn HookApi,
        context: &Context,
    ) -> Option<ContinueInput>;
    /// After every tool of the round is terminal.
    async fn after_tools(
        &self,
        assistant: EntryId,
        results: &[EntryId],
        api: &dyn HookApi,
        context: &Context,
    );
}

/// Hooks of the built-in tool task.
#[async_trait]
pub trait ToolHooks: Send + Sync {
    /// Before intent; the first block wins, otherwise `arguments` replace the call's.
    async fn before_tool(
        &self,
        call: &ToolCall,
        api: &dyn HookApi,
        context: &Context,
    ) -> Option<BeforeToolResult>;
    /// After execution, before the result entry; replaces the result.
    async fn after_tool(
        &self,
        call: &ToolCall,
        result: ToolExecutionResult,
        api: &dyn HookApi,
        context: &Context,
    ) -> Option<ToolExecutionResult>;
    /// Decorates a result when an unsafe tool's recorded execution is recovered.
    ///
    /// This recovery-only hook is separate from `after_tool`, which may carry
    /// ordinary completion side effects and is not rerun after interruption.
    async fn on_interrupted(
        &self,
        _call: &ToolCall,
        _result: ToolExecutionResult,
        _api: &dyn HookApi,
        _context: &Context,
    ) -> Option<ToolExecutionResult> {
        None
    }
}

/// `beforeCompact` input.
#[derive(Debug, Clone)]
pub struct BeforeCompact {
    /// Why the compaction runs.
    pub reason: CompactionReason,
    /// Active entries the summary replaces.
    pub entries: Vec<EntryRecord>,
    /// Their model context.
    pub messages: Vec<Message>,
    /// First entry kept verbatim.
    pub first_kept: EntryId,
    /// Manual instructions.
    pub instructions: Option<String>,
}

/// `beforeCompact` decision.
#[derive(Debug, Clone)]
pub enum BeforeCompactResult {
    /// Decline compaction.
    Decline,
    /// Supply the summary.
    Summary(String),
}

/// Hooks of the built-in compaction task.
#[async_trait]
pub trait CompactionHooks: Send + Sync {
    /// Select the model used for an internal compaction summary before its request is pinned.
    async fn select_model(
        &self,
        _current: ModelRef,
        _context: &Context,
    ) -> SessionResult<Option<ModelRef>> {
        Ok(None)
    }

    /// After range selection, before summarizing; the first decision wins.
    async fn before_compact(
        &self,
        compaction: BeforeCompact,
        api: &dyn HookApi,
        context: &Context,
    ) -> Option<BeforeCompactResult>;
}

// ---------------------------------------------------------------------------
// Typed document helpers
// ---------------------------------------------------------------------------

/// Deserialize a document value into a typed state.
pub(crate) fn read_state<T: serde::de::DeserializeOwned>(
    draft: &crate::session::DocDraft,
) -> Result<T, SessionError> {
    serde_json::from_value(draft.read()?).map_err(|error| SessionError::message(error.to_string()))
}

/// Serialize a typed state and store it into an open document draft.
pub(crate) fn write_state<T: Serialize>(
    draft: &crate::session::DocDraft,
    state: &T,
) -> Result<(), SessionError> {
    let value =
        serde_json::to_value(state).map_err(|error| SessionError::message(error.to_string()))?;
    let mut guard = draft.write()?;
    *guard = value;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tool execution API seam
// ---------------------------------------------------------------------------

/// Options for one nested tool call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolOptions {
    /// Stable key used to identify this call across a replay of its caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Suppress live progress while retaining the terminal result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<bool>,
}

/// Durable result returned to a caller of `execute_tool`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolExecutionResult {
    /// Child task identity.
    pub task_id: TaskId,
    /// Structured tool result, when returned by the nested tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<JsonValue>,
    /// Whether the nested tool reported an error.
    #[serde(default)]
    pub is_error: bool,
    /// Tool details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Diagnostics from the nested tool.
    #[serde(default)]
    pub diagnostics: Vec<ToolDiagnostic>,
    /// Usage reported by the nested tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Wall duration of the latest settled attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Operations available to one tool invocation.
#[async_trait]
pub trait ToolExecutionApi: DocumentReader {
    /// Share a handle for an asynchronous host callback while the call remains live.
    fn shared(&self) -> Arc<dyn ToolExecutionApi>;
    /// Execute one nested tool call under this task's durable ownership.
    async fn execute_tool(
        &self,
        name: &str,
        arguments: JsonObject,
        options: NestedToolOptions,
        context: &Context,
    ) -> Result<NestedToolExecutionResult, SessionError>;
    /// Read the invocation's durable clock.
    fn now(&self) -> Result<f64, SessionError>;
    /// Tool task.
    fn task_id(&self) -> TaskId;
    /// Calling conversation.
    fn conversation_id(&self) -> ConversationId;
    /// Provider call ID.
    fn call_id(&self) -> &str;
    /// The tool task's phase snapshot.
    fn registry(&self) -> Arc<dyn RegistrySnapshot>;
    /// The calling conversation's agent.
    async fn agent(&self, context: &Context) -> Result<Agent, SessionError>;
    /// pi-ai model catalog.
    fn models(&self) -> &Models;
    /// Environment built for this call, if any.
    fn env(&self) -> Option<Arc<dyn ExecutionEnv>>;
    /// Append running output; fails when the retained window cannot accept it.
    fn output(&self, chunk: &[u8], skipped: Option<OutputSkip>) -> Result<(), String>;
    /// Retained output window, if any.
    fn output_window(&self) -> Option<OutputWindow>;
    /// Record a model-visible remark.
    fn diagnostic(&self, diagnostic: ToolDiagnostic);
    /// Replace running details.
    async fn details(&self, value: JsonValue, context: &Context) -> Result<(), SessionError>;
    /// Commit one change attributed to the tool task.
    async fn commit(
        &self,
        change: ToolCommitFn,
        context: &Context,
    ) -> Result<JsonValue, SessionError>;
    /// Read one memo.
    async fn memo(&self, name: &str, context: &Context) -> Result<Option<JsonValue>, SessionError>;
    /// Write one memo when absent.
    async fn memo_set(
        &self,
        name: &str,
        value: JsonValue,
        context: &Context,
    ) -> Result<JsonValue, SessionError>;
    /// Read a committed task record (upstream `api.getTask`).
    async fn get_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> Result<Option<TaskRecord>, SessionError>;
    /// Resolve with a task's terminal receipt (upstream `api.waitForTask`).
    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &Context,
    ) -> Result<SettledTask<JsonValue>, SessionError>;
    /// Invocation-bound handle of an existing conversation (upstream `api.conversation`).
    async fn conversation(
        &self,
        id: ConversationId,
        context: &Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, SessionError>;
}
