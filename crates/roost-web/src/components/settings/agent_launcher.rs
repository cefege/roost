//! Settings → Agents → Launcher: the shared OMP auto-launch switch.
//! Reads/writes AgentConfig through client-core and updates terminal spawn state.
//! Writes are optimistic; a coordinator refusal restores the last saved switch.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::agent_config::{
    GetAgentConfig, SetAgentConfig,
};
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::ui::Switch;
use crate::pump::{Pump, use_store};

#[derive(Debug, Clone, Copy, Default)]
struct LauncherDraft {
    #[cfg(target_arch = "wasm32")]
    stored: bool,
    auto_launch: bool,
    loaded: bool,
    saving: bool,
}

#[component]
pub fn AgentLauncherPane() -> Element {
    let pump = use_store();
    let draft = use_signal(LauncherDraft::default);
    let load_pump = pump.clone();
    use_effect(move || load(load_pump.clone(), draft));
    let state = draft();
    rsx! {
        div {
            class: "max-w-xl",
            "data-testid": "agent-launcher-pane",
            p { class: "text-sm text-muted-foreground mb-4",
                "OMP is the only supported agent. Otherwise, new terminals start a normal shell."
            }
            div { class: "flex items-center gap-3",
                Switch {
                    test_id: "agent-auto-launch-toggle",
                    aria_label: "Auto-launch OMP in new terminal windows",
                    checked: state.auto_launch,
                    disabled: state.saving || !state.loaded,
                    on_checked_change: move |enabled| set_auto_launch(pump.clone(), enabled, draft),
                }
                span { class: "text-sm", "Auto-launch OMP in new terminals" }
            }
        }
    }
}

fn load(pump: Pump, draft: Signal<LauncherDraft>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut draft = draft;
        match pump.rpc().call(&GetAgentConfig).await {
            Ok(stored) => {
                pump.write_store(|store| {
                    roost_client_core::store::agent_launcher::apply_stored_agent_config(
                        store,
                        stored.auto_launch,
                    );
                });
                *draft.write() = LauncherDraft {
                    stored: stored.auto_launch,
                    auto_launch: stored.auto_launch,
                    loaded: true,
                    saving: false,
                };
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "OMP auto-launch read refused");
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("OMP auto-launch read failed: {error}"),
                }));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, draft);
}

fn set_auto_launch(pump: Pump, enabled: bool, mut draft: Signal<LauncherDraft>) {
    draft.with_mut(|state| {
        state.auto_launch = enabled;
        state.saving = true;
    });
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump
            .rpc()
            .call(&SetAgentConfig {
                auto_launch: enabled,
            })
            .await
        {
            Ok(answered) => {
                pump.write_store(|store| {
                    roost_client_core::store::agent_launcher::apply_stored_agent_config(
                        store,
                        answered.auto_launch,
                    );
                });
                let mut state = draft.write();
                state.stored = answered.auto_launch;
                state.auto_launch = answered.auto_launch;
                state.saving = false;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "OMP auto-launch write refused");
                draft.with_mut(|state| {
                    state.auto_launch = state.stored;
                    state.saving = false;
                });
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("OMP auto-launch save failed: {error}"),
                }));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, draft, enabled);
}

#[cfg(target_arch = "wasm32")]
#[component]
pub fn AgentLauncherLoader() -> Element {
    let pump = use_store();
    use_effect(move || {
        wasm_bindgen_futures::spawn_local({
            let pump = pump.clone();
            async move {
                match pump.rpc().call(&GetAgentConfig).await {
                    Ok(stored) => {
                        tracing::info!(
                            target: "settings",
                            auto_launch = stored.auto_launch,
                            "OMP auto-launch configuration read"
                        );
                        pump.write_store(|store| {
                            roost_client_core::store::agent_launcher::apply_stored_agent_config(
                                store,
                                stored.auto_launch,
                            );
                        });
                    }
                    Err(error) => {
                        tracing::warn!(target: "settings", %error, "OMP auto-launch read refused at boot");
                    }
                }
            }
        });
    });
    rsx! {}
}

#[cfg(not(target_arch = "wasm32"))]
#[component]
pub fn AgentLauncherLoader() -> Element {
    rsx! {}
}
