//! The agent harness: the conversation loop, prompts, model roles, plan mode,
//! subagents, the advisor and slash commands. The coordinator drives it through
//! the store, tool-executor, sink and model seams in `traits`.

#![forbid(unsafe_code)]
mod web_types;
pub use web_types::FetchedPage;

pub mod admission;
mod advisor;
mod ask;
mod auto_thinking;
mod btw;
mod commands;
mod compaction;
mod compaction_text;
mod context_files;
pub mod error;
mod find;
mod folder_roles;
mod goal_mode;
pub mod history;
mod job_notice;
pub mod journal;
mod judging;
mod live_item;
pub mod llm_adapter;
mod loop_mode;
pub mod memory_store;
mod model_call;
mod model_presets;
mod model_switch;
mod peer_message;
mod peer_notice;
mod plan_mode;
pub mod projection;
pub mod prompts;
mod read_only_guard;
pub mod records;
mod rewind;
pub mod roles;
mod run_loop;
mod runtime;
mod subagents;
mod todo;
mod todo_notice;
mod tool_round;
pub mod toolset;
pub mod traits;
mod turn;
mod unexpected_stop;
mod wait;
mod web_adapter;
mod web_tools;

pub use error::AgentError;
pub use llm_adapter::RoostLlm;
pub use memory_store::InMemoryAgentStore;
pub use records::{AgentSettings, ConversationRecord, Entry, Mode, Role};
pub use runtime::{AgentRuntime, NewConversation, RuntimeConfig, SubmitMode};
pub use traits::{
    AccountUsage, AdmitOutcome, AgentStore, ChatSink, ClusterSignal, CommandAdmission,
    IntentChange, LeaseFence, Llm, NoClusterSignal, OwnerConversationId, ToolCall, ToolExecutor,
    ToolOutcome, WorkerConversationNamespace,
};
