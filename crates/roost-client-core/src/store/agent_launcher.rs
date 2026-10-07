//! The default-agent launch configuration this browser holds, and the command
//! a new terminal auto-launches. Ported from the reading half of
//! `apps/web/src/lib/agents.ts` (`resolveAgentFrom`, `autoLaunchEnabled`,
//! `loadAgentConfig`'s store writes): the coordinator owns the stored value —
//! `AgentConfigGet`/`AgentConfigSet` — and this module holds the ANSWER so a
//! deck spawn can read it without re-dialling. The Settings launcher pane
//! writes the store through [`apply_stored_agent_config`] too, so the value the
//! pane shows and the value a spawn reads cannot disagree.
//!
//! The command is derived, never stored: the catalog lives in roost-web's
//! settings picker, the coordinator stores raw strings, and a client resolves
//! an id it does not know to the same default a blank selection gets.

use crate::store::Store;

/// The coordinator's agent-config answer, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentLauncherState {
    /// A built-in agent id, or `"custom"`.
    pub selected: String,
    /// The custom launch string; empty when unset.
    pub custom_command: String,
    /// Whether a new terminal auto-launches the agent.
    pub auto_launch: bool,
    /// Whether a coordinator has answered at least once. Before it has, the
    /// stored fields are the shipped defaults and nothing may launch, because
    /// launching on defaults would run an agent the reader never chose.
    pub loaded: bool,
}

impl AgentLauncherState {
    /// The command this configuration launches, or `None` for a plain shell.
    ///
    /// `"custom"` with a blank command is a selection the pane has not saved a
    /// command for yet, so nothing launches rather than the default — the
    /// reader asked for a command they have not typed.
    pub fn launch_command(&self) -> Option<String> {
        if !self.loaded || !self.auto_launch {
            return None;
        }
        resolve_launch_command(&self.selected, &self.custom_command)
    }
}

/// The command a stored selection means, regardless of the auto-launch switch.
///
/// A blank selection is the coordinator's own fallback answer and means OMP;
/// `"custom"` trims the command and may name nothing.
pub fn resolve_launch_command(selected: &str, custom_command: &str) -> Option<String> {
    if selected == "custom" {
        let command = custom_command.trim();
        return if command.is_empty() {
            None
        } else {
            Some(command.to_owned())
        };
    }
    let command = selected.trim();
    if command.is_empty() {
        Some(OMP_LAUNCH_COMMAND.to_owned())
    } else {
        Some(command.to_owned())
    }
}

/// The agent a blank selection launches: the coordinator's fallback default.
pub const OMP_LAUNCH_COMMAND: &str = "omp";

/// Store one coordinator answer, replacing whatever was held.
pub fn apply_stored_agent_config(
    store: &mut Store,
    selected: String,
    custom_command: String,
    auto_launch: bool,
) -> bool {
    let changed = {
        let state = &store.agent_launcher;
        state.selected != selected
            || state.custom_command != custom_command
            || state.auto_launch != auto_launch
            || !state.loaded
    };
    store.agent_launcher = AgentLauncherState {
        selected,
        custom_command,
        auto_launch,
        loaded: true,
    };
    changed
}

/// Drop the configuration: the credential boundary discards every stored
/// answer, and a response from before the boundary must not launch an agent.
pub fn discard_agent_launcher_config(store: &mut Store) {
    store.agent_launcher = AgentLauncherState::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::MemoryKeyValueStore;
    use crate::store::Store;
    use crate::sync::link::SyncState;

    fn empty_store() -> Store {
        Store::new(SyncState::new(&MemoryKeyValueStore::new()), "tab")
    }

    #[test]
    fn an_unloaded_store_launches_nothing() {
        assert_eq!(empty_store().agent_launcher.launch_command(), None);
    }

    #[test]
    fn a_loaded_auto_launch_selection_resolves_its_command() {
        let mut store = empty_store();
        apply_stored_agent_config(&mut store, "claude".to_owned(), String::new(), true);
        assert_eq!(
            store.agent_launcher.launch_command().as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn the_switch_off_means_a_plain_shell() {
        let mut store = empty_store();
        apply_stored_agent_config(&mut store, "claude".to_owned(), String::new(), false);
        assert_eq!(store.agent_launcher.launch_command(), None);
    }

    #[test]
    fn a_blank_selection_launches_the_omp_default() {
        assert_eq!(
            resolve_launch_command("", ""),
            Some(OMP_LAUNCH_COMMAND.to_owned())
        );
    }

    #[test]
    fn a_custom_command_is_trimmed_and_a_blank_custom_launches_nothing() {
        assert_eq!(
            resolve_launch_command("custom", "  aider --model sonnet "),
            Some("aider --model sonnet".to_owned())
        );
        assert_eq!(resolve_launch_command("custom", "   "), None);
    }

    #[test]
    fn a_discarded_configuration_launches_nothing_again() {
        let mut store = empty_store();
        apply_stored_agent_config(&mut store, "claude".to_owned(), String::new(), true);
        discard_agent_launcher_config(&mut store);
        assert_eq!(store.agent_launcher.launch_command(), None);
    }
}
