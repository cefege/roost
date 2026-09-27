//! Coding-agent status on the client: the fence, the retained rows, the
//! acknowledgement ledger, and the vocabulary every surface reads.
//!
//! Three files, three owners, and the order matters: `status_policy` derives
//! what a status MEANS, `seen` remembers what this profile has acknowledged,
//! and `status_projection` decides which report may replace a row. The fence
//! itself is `roost_protocol::wire::agent_status::order`, shared with the
//! coordinator, so neither end can drift from the other.

pub mod seen;
pub mod status_policy;
pub mod status_projection;

pub use seen::{AGENT_SEEN_STORAGE_KEY, AgentSeenLedger};
pub use status_policy::{
    AgentDotStatus, AgentStatusCounts, AgentStatusLevel, AgentStatusPresentation,
    AgentStatusRevisionToken, AgentStatusRollup, agent_status_completion_unseen,
    agent_status_level_token, agent_status_level_token_for, agent_status_occupant_key,
    agent_status_presentation, agent_status_revision_token, agent_status_tooltip,
    derive_agent_status_level, fold_agent_status_levels, format_agent_status_counts,
    matches_agent_status_revision_token,
};
pub use status_projection::{AgentStatusChange, AgentStatusProjection, CLOSED_SESSION_FENCE_MAX};
