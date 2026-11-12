//! OMP auto-launch state read by terminal spawns and the Settings pane.
//! The coordinator owns the switch; no launch is allowed before it answers.
//! Credential changes discard the answer so a previous account cannot launch.

use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentLauncherState {
    pub auto_launch: bool,
    pub loaded: bool,
}

impl AgentLauncherState {
    pub fn launch_command(&self) -> Option<&'static str> {
        (self.loaded && self.auto_launch).then_some("omp")
    }
}

pub fn apply_stored_agent_config(store: &mut Store, auto_launch: bool) -> bool {
    let changed = store.agent_launcher.auto_launch != auto_launch || !store.agent_launcher.loaded;
    store.agent_launcher = AgentLauncherState {
        auto_launch,
        loaded: true,
    };
    changed
}

pub fn discard_agent_launcher_config(store: &mut Store) {
    store.agent_launcher = AgentLauncherState::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::MemoryKeyValueStore;
    use crate::sync::link::SyncState;

    fn empty_store() -> Store {
        Store::new(SyncState::new(&MemoryKeyValueStore::new()), "tab")
    }

    #[test]
    fn no_launch_before_configuration_is_loaded() {
        let mut store = empty_store();
        store.agent_launcher.auto_launch = true;
        assert_eq!(store.agent_launcher.launch_command(), None);
    }

    #[test]
    fn loaded_switch_controls_omp_and_plain_shell_launches() {
        let mut store = empty_store();
        apply_stored_agent_config(&mut store, true);
        assert_eq!(store.agent_launcher.launch_command(), Some("omp"));
        apply_stored_agent_config(&mut store, false);
        assert_eq!(store.agent_launcher.launch_command(), None);
    }

    #[test]
    fn credential_boundary_revokes_auto_launch_until_another_answer() {
        let mut store = empty_store();
        apply_stored_agent_config(&mut store, true);
        discard_agent_launcher_config(&mut store);
        assert_eq!(store.agent_launcher.launch_command(), None);
        apply_stored_agent_config(&mut store, true);
        assert_eq!(store.agent_launcher.launch_command(), Some("omp"));
    }
}
