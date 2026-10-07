//! The agents a new terminal can auto-launch, and how a selection resolves to
//! the command that runs. Ports `apps/web/src/lib/agents.ts`'s `BUILTIN_AGENTS`
//! and `resolveAgentFrom`; the Settings launcher pane is the only reader.
//!
//! The colour each definition carried in v2 is deliberately absent: it fed a
//! glyph tile, and this build draws the resolved agent as a `Chip` over the
//! primitive palette, so a second copy of the product palette would be a value
//! nothing reads.

/// One launchable agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDef {
    /// The stored selection.
    pub id: &'static str,
    /// What the reader sees.
    pub label: &'static str,
    /// The command the agent runs.
    pub command: &'static str,
    /// The tile glyph.
    pub glyph: &'static str,
}

/// Every built-in agent, in the picker's order.
pub const BUILTIN_AGENTS: [AgentDef; 11] = [
    AgentDef {
        id: "codex",
        label: "OpenAI Codex",
        command: "codex",
        glyph: "Cx",
    },
    AgentDef {
        id: "claude",
        label: "Claude Code",
        command: "claude",
        glyph: "C",
    },
    AgentDef {
        id: "gemini",
        label: "Gemini CLI",
        command: "gemini",
        glyph: "G",
    },
    AgentDef {
        id: "opencode",
        label: "OpenCode",
        command: "opencode",
        glyph: "OC",
    },
    AgentDef {
        id: "cursor",
        label: "Cursor Agent",
        command: "cursor-agent",
        glyph: "Cu",
    },
    AgentDef {
        id: "amp",
        label: "Amp",
        command: "amp",
        glyph: "A",
    },
    AgentDef {
        id: "copilot",
        label: "GitHub Copilot CLI",
        command: "copilot",
        glyph: "Co",
    },
    AgentDef {
        id: "droid",
        label: "Droid",
        command: "droid",
        glyph: "D",
    },
    AgentDef {
        id: "grok",
        label: "Grok CLI",
        command: "grok",
        glyph: "Gr",
    },
    AgentDef {
        id: "pi",
        label: "Pi",
        command: "pi",
        glyph: "π",
    },
    AgentDef {
        id: "omp",
        label: "OMP",
        command: "omp",
        glyph: "O",
    },
];

/// The stored spelling of a free-text launch command.
pub const CUSTOM_AGENT_ID: &str = "custom";

/// The agent a store with no selection launches.
pub const DEFAULT_AGENT: AgentDef = AgentDef {
    id: "omp",
    label: "OMP",
    command: "omp",
    glyph: "O",
};

/// The definition behind a stored id, or the default.
pub fn agent_by_id(id: &str) -> AgentDef {
    BUILTIN_AGENTS
        .iter()
        .copied()
        .find(|agent| agent.id == id)
        .unwrap_or(DEFAULT_AGENT)
}

/// What a selection actually launches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgent {
    /// The stored selection this came from.
    pub id: String,
    /// What the reader sees.
    pub label: String,
    /// The command, with the enter key the preview appends.
    pub command: String,
    /// The tile glyph.
    pub glyph: String,
    /// Whether this is a free-text command rather than a built-in.
    pub is_custom: bool,
}

/// Resolve a selection and its free-text command into one launchable agent.
///
/// An empty custom command resolves to the default rather than to nothing: a
/// reader who chose "Custom command…" and typed nothing has not chosen a
/// command, and launching the default is what the store already says.
pub fn resolve_agent(selected: &str, custom: &str) -> ResolvedAgent {
    if selected == CUSTOM_AGENT_ID {
        let command = custom.trim();
        if command.is_empty() {
            return built_in(&DEFAULT_AGENT, false);
        }
        let glyph = command
            .chars()
            .next()
            .map(|first| first.to_uppercase().to_string())
            .unwrap_or_default();
        return ResolvedAgent {
            id: CUSTOM_AGENT_ID.to_owned(),
            label: "Custom".to_owned(),
            command: command.to_owned(),
            glyph,
            is_custom: true,
        };
    }
    built_in(&agent_by_id(selected), false)
}

fn built_in(agent: &AgentDef, is_custom: bool) -> ResolvedAgent {
    ResolvedAgent {
        id: agent.id.to_owned(),
        label: agent.label.to_owned(),
        command: agent.command.to_owned(),
        glyph: agent.glyph.to_owned(),
        is_custom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_custom_selection_with_no_command_launches_the_default() {
        assert_eq!(
            resolve_agent(CUSTOM_AGENT_ID, "   "),
            built_in(&DEFAULT_AGENT, false)
        );
    }

    #[test]
    fn a_custom_command_is_trimmed_and_labelled() {
        let resolved = resolve_agent(CUSTOM_AGENT_ID, "  aider --model sonnet ");
        assert_eq!(resolved.command, "aider --model sonnet");
        assert_eq!(resolved.label, "Custom");
        assert_eq!(resolved.glyph, "A");
        assert!(resolved.is_custom);
    }

    #[test]
    fn an_unknown_id_is_the_default_agent() {
        assert_eq!(agent_by_id("nope").id, DEFAULT_AGENT.id);
    }
}
