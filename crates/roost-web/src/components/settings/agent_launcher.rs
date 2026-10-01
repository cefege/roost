//! Settings → Agents → Launcher: which agent a new terminal auto-launches.
//!
//! Ports `apps/web/src/components/Settings/AgentLauncherPane.tsx` and the
//! `saveAgentConfig`/`saveAutoLaunch` half of `apps/web/src/lib/agents.ts`.
//! Depends on `roost-client-core`'s `AgentConfigGet`/`AgentConfigSet` calls; the
//! stored value lives on the coordinator, so the choice applies to every device.
//!
//! The pane is OPTIMISTIC and ROLLBACK-SAFE: a control shows the reader's
//! choice at once and restores the stored one if the coordinator refuses the
//! write, which is the behaviour the two browser regressions in
//! `smoke/terminal/agent-launcher-rejection.spec.ts` pin.

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::settings::agent_config::AgentLauncherConfig;
// The round trips are browser-only: a native build has no coordinator to ask,
// so the call types and the toast their refusals raise are gated with them.
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::agent_config::{
    GetAgentConfig, SetAgentConfig,
};
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;

use super::agents::{BUILTIN_AGENTS, CUSTOM_AGENT_ID, DEFAULT_AGENT, ResolvedAgent, resolve_agent};
use crate::components::md::{Button, ButtonVariant, Chip, Select, SelectOption, Switch, TextField};
use crate::pump::{Pump, use_store};

/// The selection the coordinator holds, plus what is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LauncherDraft {
    stored: AgentLauncherConfig,
    selected: String,
    custom: String,
    auto_launch: bool,
    saving: bool,
}

impl LauncherDraft {
    /// Before the coordinator has answered: the shipped default, so the pane
    /// renders controls rather than waiting to draw them.
    fn initial() -> Self {
        Self {
            stored: AgentLauncherConfig::default(),
            selected: DEFAULT_AGENT.id.to_owned(),
            custom: String::new(),
            auto_launch: false,
            saving: false,
        }
    }

    /// The agent this draft would launch.
    fn resolved(&self) -> ResolvedAgent {
        resolve_agent(&self.selected, &self.custom)
    }
}

/// The pane.
#[component]
pub fn AgentLauncherPane() -> Element {
    let pump = use_store();
    let mut draft = use_signal(LauncherDraft::initial);
    // The effect outlives this render, so it takes its OWN clone: a `move`
    // closure over `pump` itself would consume the binding the three control
    // handlers below still read.
    let load_pump = pump.clone();
    use_effect(move || load(load_pump.clone(), draft));

    let selected = draft().selected;
    let custom = draft().custom;
    let auto_launch = draft().auto_launch;
    let saving = draft().saving;
    let resolved = draft().resolved();
    let options: Vec<SelectOption> = BUILTIN_AGENTS
        .iter()
        .map(|agent| SelectOption::new(agent.id, agent.label))
        .chain(std::iter::once(SelectOption::new(
            CUSTOM_AGENT_ID,
            "Custom command…",
        )))
        .collect();
    // One clone per control: three `move` closures over the same non-`Copy`
    // binding would consume it on the first, and a handler that runs on click
    // must not re-borrow the pump the render is still holding.
    let choose_pump = pump.clone();
    let save_pump = pump.clone();
    let auto_launch_pump = pump.clone();
    rsx! {
        div {
            class: "settings-pane",
            style: "max-width: 560px;",
            "data-testid": "agent-launcher-pane",
            p { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant); margin: 0 0 var(--md-space-4);",
                "Pick the agent Roost auto-launches in new terminals. Applies to every device."
            }
            Select {
                test_id: "agent-select",
                label: "Default agent",
                value: selected.clone(),
                options,
                disabled: saving,
                on_change: move |value| choose(choose_pump.clone(), value, draft),
            }
            if selected == CUSTOM_AGENT_ID {
                div { style: "display: flex; align-items: flex-end; gap: var(--md-space-3); margin-block-start: var(--md-space-4);",
                    TextField {
                        test_id: "agent-custom-command",
                        label: "Custom command",
                        placeholder: "e.g. aider --model sonnet",
                        style: "flex: 1 1 auto;",
                        value: custom.clone(),
                        on_input: move |value| draft.write().custom = value,
                    }
                    Button {
                        variant: ButtonVariant::Default,
                        "data-testid": "agent-custom-save",
                        disabled: saving || custom.trim().is_empty(),
                        onclick: move |_| save_custom(save_pump.clone(), draft),
                        "Save"
                    }
                }
            }
            div { style: "display: flex; align-items: center; gap: var(--md-space-3); margin-block-start: var(--md-space-5);",
                Chip { label: resolved.glyph.clone(), icon: "extension" }
                span { class: "md-body-m", style: "font-weight: 600;", {resolved.label.clone()} }
                code { style: "font-family: var(--font-mono);",
                    {format!("{}\u{23ce}", resolved.command)}
                }
            }
            div { style: "display: flex; align-items: center; gap: var(--md-space-3); margin-block-start: var(--md-space-5);",
                Switch {
                    test_id: "agent-auto-launch-toggle",
                    label: "Auto-launch agent in new terminal windows",
                    checked: auto_launch,
                    disabled: saving,
                    on_change: move |enabled| set_auto_launch(auto_launch_pump.clone(), enabled, draft),
                }
            }
        }
    }
}

/// Read the stored configuration once, before the controls are trusted.
fn load(pump: Pump, draft: Signal<LauncherDraft>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut draft = draft;
        match pump.rpc().call(&GetAgentConfig).await {
            Ok(stored) => {
                let mut draft = draft.write();
                draft.selected = stored.selected.clone();
                draft.custom = stored.custom_command.clone();
                draft.auto_launch = stored.auto_launch;
                draft.stored = stored;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "agent config read refused");
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("Default agent read failed: {error}"),
                }));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, draft);
}

/// A built-in selection saves immediately; "Custom command…" only reveals the
/// text field, because free text needs an explicit Save.
fn choose(pump: Pump, value: String, mut draft: Signal<LauncherDraft>) {
    if value == CUSTOM_AGENT_ID {
        draft.write().selected = value;
        return;
    }
    commit(
        pump,
        draft,
        AgentLauncherConfig {
            selected: value,
            custom_command: draft().custom.clone(),
            auto_launch: draft().auto_launch,
        },
        "Default agent save failed: ",
        Some("Default agent saved"),
    );
}

/// Save the free-text command as the launch command.
fn save_custom(pump: Pump, draft: Signal<LauncherDraft>) {
    let custom = draft().custom.trim().to_owned();
    if custom.is_empty() {
        return;
    }
    commit(
        pump,
        draft,
        AgentLauncherConfig {
            selected: CUSTOM_AGENT_ID.to_owned(),
            custom_command: custom,
            auto_launch: draft().auto_launch,
        },
        "Default agent save failed: ",
        Some("Default agent saved"),
    );
}

/// The auto-launch switch, whose failure message names the switch.
fn set_auto_launch(pump: Pump, enabled: bool, mut draft: Signal<LauncherDraft>) {
    draft.write().auto_launch = enabled;
    commit(
        pump,
        draft,
        AgentLauncherConfig {
            selected: draft().selected.clone(),
            custom_command: draft().custom.clone(),
            auto_launch: enabled,
        },
        "Auto-launch save failed: ",
        None,
    );
}

/// Write the whole configuration, showing the reader's choice at once and
/// putting the stored one back when the coordinator refuses.
///
/// `success` is `None` for the auto-launch switch, which v2 confirmed by the
/// switch moving rather than by a card.
fn commit(
    pump: Pump,
    mut draft: Signal<LauncherDraft>,
    wanted: AgentLauncherConfig,
    failure_prefix: &'static str,
    success: Option<&'static str>,
) {
    draft.write().saving = true;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let request = SetAgentConfig {
            selected: wanted.selected.clone(),
            custom_command: wanted.custom_command.clone(),
            auto_launch: wanted.auto_launch,
        };
        let stored = draft.peek().stored.clone();
        match pump.rpc().call(&request).await {
            Ok(answered) => {
                {
                    let mut draft = draft.write();
                    draft.selected = answered.selected.clone();
                    draft.custom = answered.custom_command.clone();
                    draft.auto_launch = answered.auto_launch;
                    draft.stored = answered;
                    draft.saving = false;
                }
                if let Some(message) = success {
                    pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                        message: message.to_owned(),
                    }));
                }
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "agent config write refused");
                let mut draft = draft.write();
                draft.selected = stored.selected;
                draft.custom = stored.custom_command;
                draft.auto_launch = stored.auto_launch;
                draft.saving = false;
                drop(draft);
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("{failure_prefix}{error}"),
                }));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, draft, wanted, failure_prefix, success);
}
