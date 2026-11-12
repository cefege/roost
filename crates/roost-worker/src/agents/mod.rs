//! Which coding agent, if any, is running in a session: the process scan, the
//! manifest rules that read a pane's screen, the report server an integration
//! pushes status into, and the prompt control that answers one. The status
//! projection itself is NOT here — `roost_protocol::wire::agent_status` owns
//! its shape, `crate::agent_occupancy` holds the occupancy shapes and
//! `agents::registry` drives them.
//! Depends on `roost_protocol` for the vocabulary — and on nothing here.
//!
//! OSC title and progress evidence is read off the same PTY stream the terminal
//! parses, and lives on the session record
//! ([`crate::session::agent_osc::AgentOscState`]) rather than here: it is a
//! property of the BYTES, not of the agent, and a copy would be a second place
//! to look for the same title.
//! Ports v2 `apps/worker/src/agents/process-scan.ts`, `apps/worker/src/agents/report-protocol.ts`.

pub mod detector;
pub mod environment;
mod file_identity;
pub mod install_integrations;
mod install_mutation;
pub mod install_proof;
mod install_stage;
pub mod install_transaction;
pub mod integration_assets;
pub mod manifest_engine;
mod manifest_regex;
mod manifest_syntax;
pub mod manifests;
pub mod peer_process_id;
pub mod process_identity;
pub mod process_scan;
pub mod process_snapshot;
#[cfg(windows)]
pub mod process_snapshot_windows;
pub mod process_tree;
pub mod prompt_control;
pub mod prompt_fence;
pub mod prompt_port;
pub mod prompt_submit;
pub mod registry;
mod registry_recompute;
pub mod report_admission;
mod report_connection;
pub mod report_protocol;
pub mod report_server;
pub mod stable_detection;
pub mod status_stack;

/// The supported terminal agent. Other commands remain ordinary terminals
/// without agent-status recognition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinAgentId {
    Omp,
}

impl BuiltinAgentId {
    /// Every supported terminal agent.
    pub const ALL: [BuiltinAgentId; 1] = [Self::Omp];

    /// The wire name an integration reports and a client matches.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Omp => "omp",
        }
    }

    /// The agent a name refers to, or `None` when it names none of them.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| agent.as_str() == name)
    }
}
