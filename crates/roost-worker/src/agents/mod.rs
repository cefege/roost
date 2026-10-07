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

pub mod conversation_recovery;
pub mod conversation_restore;
pub mod detector;
pub mod environment;
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
mod manifests_claude;
pub mod peer_process_id;
pub mod process_identity;
pub mod process_scan;
pub mod process_snapshot;
pub mod process_tree;
pub mod prompt_control;
pub mod prompt_fence;
pub mod prompt_port;
pub mod prompt_submit;
pub mod reference_admission;
pub mod registry;
mod registry_recompute;
pub mod report_admission;
mod report_connection;
pub mod report_protocol;
pub mod report_server;
pub mod stable_detection;
pub mod status_stack;

/// How many distinct built-in agents this worker can recognise.
///
/// Closed on purpose. An unrecognised agent is an agent this worker reports no
/// status for, which is a state a client already handles; a fifth value that
/// arrived from a newer peer would be a state it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinAgentId {
    Codex,
    Claude,
    Gemini,
    OpenCode,
    Cursor,
    Amp,
    Copilot,
    Droid,
    Grok,
    Pi,
    Omp,
}

impl BuiltinAgentId {
    /// Every built-in agent, in the order the manifests are evaluated.
    pub const ALL: [BuiltinAgentId; 11] = [
        Self::Codex,
        Self::Claude,
        Self::Gemini,
        Self::OpenCode,
        Self::Cursor,
        Self::Amp,
        Self::Copilot,
        Self::Droid,
        Self::Grok,
        Self::Pi,
        Self::Omp,
    ];

    /// The wire name an integration reports and a client matches on. These are
    /// v2's `BUILTIN_AGENT_COMMANDS` keys verbatim: an alias the command line
    /// answers to (`open-code`, `ghcs`) is matched when scanning and is NOT a
    /// second agent, so publishing it would be a second id for one agent.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::Amp => "amp",
            Self::Copilot => "copilot",
            Self::Droid => "droid",
            Self::Grok => "grok",
            Self::Pi => "pi",
            Self::Omp => "omp",
        }
    }

    /// The agent a name refers to, or `None` when it names none of them.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| agent.as_str() == name)
    }
}
