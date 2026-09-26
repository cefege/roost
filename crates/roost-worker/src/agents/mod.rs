//! Which coding agent, if any, is running in a session: the process scan, the
//! manifest rules that read a pane's screen, the report server an integration
//! pushes status into, and the prompt control that answers one. The status
//! projection itself is NOT here — `roost_protocol::wire::agent_status` owns
//! its shape, and `crate::agent_occupancy` owns who occupies a session.
//! Depends on `roost_protocol` for the vocabulary — and on nothing here.
//!
//! OSC title and progress evidence is read off the same PTY stream the terminal
//! parses, and lives on the session record
//! ([`crate::session::agent_osc::AgentOscState`]) rather than here: it is a
//! property of the BYTES, not of the agent, and a copy would be a second place
//! to look for the same title.

/// How many distinct built-in agents this worker can recognise.
///
/// Closed on purpose. An unrecognised agent is an agent this worker reports no
/// status for, which is a state a client already handles; a fifth value that
/// arrived from a newer peer would be a state it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinAgentId {
    Codex,
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
    pub const ALL: [BuiltinAgentId; 10] = [
        Self::Codex,
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

/// The largest request line the agent report protocol accepts.
///
/// The report endpoint is a loopback server any local process can reach, so the
/// bound is on the LINE rather than on the parsed message: a request that never
/// completes costs a connection, not a buffer.
pub const AGENT_REPORT_MAX_LINE_BYTES: usize = 32 * 1024;
