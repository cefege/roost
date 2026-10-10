//! The LLM provider layer of the agent harness: the model catalog, provider wire clients,
//! OAuth logins, the multi-account credential pool, usage reports and the judge.
//! Called by roost-agent and the coordinator; depends on reqwest and roost-observability.

#![forbid(unsafe_code)]

pub mod auth;
pub mod catalog;
pub mod credentials;
pub mod endpoints;
pub mod error;
pub mod judge;
pub mod memory_store;
pub mod message;
pub mod oauth;
pub mod pool;
pub mod providers;
pub mod rotation;
pub mod usage;
pub use providers::{HeaderObserver, stream_chat};

pub use auth::ResolvedAuth;
pub use catalog::{Catalog, Cost, ModelInfo, ModelKind, THINKING_LEVELS, WireApi};
pub use credentials::{CredentialKind, CredentialStore, StoredCredential};
pub use endpoints::Endpoints;
pub use error::LlmError;
pub use judge::{Answer, Judge, Question, QuestionKind};
pub use memory_store::InMemoryCredentialStore;
pub use message::{
    AssistantBlock, BlockKind, ChatRequest, Message, StopReason, StreamEvent, ToolSpec, Usage,
    UserContent,
};
pub use oauth::{LoginSession, LoginView, NewCredential};
pub use pool::AccountPool;
pub use usage::{UsageReport, UsageWindow};
