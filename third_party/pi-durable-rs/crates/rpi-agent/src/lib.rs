//! `rpi-agent` — port of `@earendil-works/pi-agent-core` @ pi 0.82.1 (2efa728).
//!
//! Agent loop, state, events, and the optional harness layer (ADR-0003 §1).
//! Depends only on `rpi-ai`'s type layer — never on concrete provider
//! implementations; streaming is injected via [`StreamFn`] (coding-standards
//! §2.2, §4.2).
//!
//! Module mapping notes:
//! - [`types`] mirrors `packages/agent/src/types.ts`.
//! - [`stream_fn`] mirrors `packages/agent/src/stream-fn.ts` (shape pinned by
//!   design doc §4.4).
//! - [`agent_loop`] mirrors `packages/agent/src/agent-loop.ts` plus the
//!   loop-facing hook types of `types.ts`.
//! - [`agent`] mirrors `packages/agent/src/agent.ts` plus `AgentState`.
//! - [`messages`] carries the `AgentMessage` union including the coding-agent
//!   custom message types from `packages/coding-agent/src/core/messages.ts`:
//!   TS declaration merging has no Rust equivalent, so the custom variants are
//!   folded into the union here (structural, not behavioral, difference).
//! - [`session`] consolidates the session entry types of
//!   `packages/coding-agent/src/core/session-manager.ts` and
//!   `packages/agent/src/harness/types.ts` into one serde skeleton shared by
//!   the `rpi` main path and the harness layer.
//! - [`harness`] mirrors `packages/agent/src/harness/` (T16 first block: the
//!   `types.ts` type layer; storage implementations and the session classes
//!   land in later T16 blocks).

pub mod agent;
pub mod agent_loop;
pub mod compaction;
pub mod error;
pub mod harness;
pub mod messages;
pub mod nested_tool_calls;
pub mod proxy;
pub mod session;
pub mod stream_fn;
pub mod types;

pub use agent::{
    Agent, AgentListener, AgentOptions, AgentState, FinishTurnAgentFn, InitialAgentState,
    PrepareRequestAgentFn, PromptInput,
};
pub use agent_loop::{
    AfterToolCallContext, AfterToolCallFn, AfterToolCallResult, AgentContext, AgentEventSink,
    AgentEventStream, AgentLoopConfig, AgentLoopTurnUpdate, AgentRequestUpdate,
    AgentToolCallOutcome, AgentTurnContext, AgentTurnDecision, BeforeToolCallContext,
    BeforeToolCallFn, BeforeToolCallResult, ConvertToLlmFn, FinishTurnFn, GetApiKeyFn,
    GetQueuedMessagesFn, NestedCallSummaryFn, PrepareNextTurnContext, PrepareNextTurnFn,
    PrepareRequestContext, PrepareRequestFn, RunToolCallOptions, TransformContextFn, agent_loop,
    agent_loop_continue, run_agent_loop, run_agent_loop_continue, run_tool_call,
};
pub use compaction::{
    CompactionDetails, CompactionPreparation, CompactionResult, CompactionSettings, CutPointResult,
    DEFAULT_COMPACTION_SETTINGS, SummarizationArgs, compact, estimate_context_tokens,
    estimate_messages_tokens, estimate_tokens, find_cut_point, find_turn_start_index,
    generate_summary, generate_summary_with_usage, get_last_assistant_usage, prepare_compaction,
    should_compact,
};
pub use error::AgentError;
pub use harness::{
    AbortResult, AgentHarness, AgentHarnessError, AgentHarnessErrorCode, AgentHarnessEvent,
    AgentHarnessHook, AgentHarnessListener, AgentHarnessOptions, AgentHarnessOwnEvent,
    AgentHarnessPhase, AgentHarnessPromptOptions, AgentHarnessResources, AgentHarnessStreamOptions,
    AgentHarnessStreamOptionsPatch, AgentHarnessSystemPrompt, AgentHarnessTool,
    AgentHarnessToolContextSource, BeforeAgentStartResult, BranchSummaryError,
    BranchSummaryErrorCode, BranchSummaryResult, ChunkCallback, CompactResult, CompactionError,
    CompactionErrorCode, ContextResult, CreateDirOptions, CreateTempFileOptions, ExecutionEnv,
    ExecutionError, ExecutionErrorCode, FileError, FileErrorCode, FileInfo, FileKind,
    FileOperations, FileSystem, ForkPosition, GenerateBranchSummaryOptions, HarnessHookResult,
    JsonlSessionCreateOptions, JsonlSessionListOptions, JsonlSessionMetadata, JsonlSessionRepoApi,
    NavigateTreeOptions, NavigateTreeResult, PatchMap, PendingSessionWrite, PromptTemplate,
    ReadTextLinesOptions, RemoveOptions, RetryOperation, Session, SessionBeforeCompactResult,
    SessionBeforeTreeResult, SessionContext, SessionCreateOptions, SessionEntryCursorOptions,
    SessionError, SessionErrorCode, SessionForkOptions, SessionMetadata, SessionModelRef,
    SessionRepo, SessionStats, SessionStorage, Shell, ShellExecOptions, ShellExecResult, Skill,
    SystemPromptContext, ToolCallResult, ToolResultPatch, TreePreparation, TreeSummary, TurnState,
    UpdateSource, apply_stream_options_patch,
};
pub use messages::{
    AgentMessage, BRANCH_SUMMARY_PREFIX, BRANCH_SUMMARY_SUFFIX, BashExecutionMessage,
    BranchSummaryMessage, COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX,
    CompactionSummaryMessage, CustomMessage, bash_execution_to_text, convert_to_llm,
};
pub use proxy::{ProxyAssistantMessageEvent, ProxyStreamOptions, stream_proxy};
pub use stream_fn::{BoxStream, StreamFn};
pub use types::{
    AgentEvent, AgentTool, AgentToolResult, AgentToolUpdateCallback, QueueMode, ToolExecutionMode,
};
