//! Settings → Agents → MCP: the relay registry.
//!
//! Ports `apps/web/src/components/Settings/McpPane.tsx` and the inline
//! `apps/web/src/components/agents/McpRelayEditor.tsx` it opens. Depends on
//! `roost-client-core`'s `McpList`/`McpCreate`/`McpDelete` calls and on the
//! `md` primitives.
//!
//! The rendered list is seeded from the store's `mcp_relays` projection — the
//! same rows v2's root store held — and re-read from `McpList`, so a
//! coordinator that REFUSES the read shows its refusal beside the projection
//! instead of an empty pane that looks like "you have no relays".

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::mcp::{
    CreateMcpRelay, DeleteMcpRelay, ListMcpRelays,
};
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;
use roost_protocol::wire::McpRelay;

use crate::components::md::{
    Button, ButtonVariant, Card, EmptyState, Icon, List, ListRow, Select, SelectOption, TextField,
};
use crate::pump::{Pump, use_store};

/// The relay list, oldest first, from the store's projection.
fn projected_relays(pump: &Pump) -> Vec<McpRelay> {
    let core = pump.core();
    let core = core.borrow();
    let mut relays: Vec<McpRelay> = core.store().mcp_relays.values().cloned().collect();
    relays.sort_by_key(|relay| relay.created_at_ms);
    relays
}

/// Everything the pane mutates, so a refusal and a row change land in the same
/// place the pane reads from.
#[derive(Debug, Clone, PartialEq, Default)]
struct McpView {
    relays: Vec<McpRelay>,
    load_error: Option<String>,
    delete_error: Option<String>,
}

/// The pane.
#[component]
pub fn McpPane() -> Element {
    let pump = use_store();
    let view = use_signal(|| McpView {
        relays: projected_relays(&pump),
        ..McpView::default()
    });
    let mut editing = use_signal(|| false);
    let confirming = use_signal(|| None::<String>);
    let editor_pump = pump.clone();
    let row_pump = pump.clone();
    use_effect(move || reload(pump.clone(), view));

    let has_relays = !view().relays.is_empty();
    let editor_open = editing();
    let on_save = {
        let editor_pump = editor_pump.clone();
        move |(label, kind, config): (String, String, String)| {
            create_relay(editor_pump.clone(), label, kind, config, view);
            editing.set(false);
        }
    };
    // The retry and the rows both need the pump, and an `rsx!` attribute body
    // is a block: one capture cannot be moved into each of them.
    let on_reload = {
        let row_pump = row_pump.clone();
        move |_event: MouseEvent| reload(row_pump.clone(), view)
    };
    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "settings-mcp-pane",
            Card {
                title: "MCP relays",
                supporting: "Bridge agents to external tools and data sources. Each relay points at an MCP server (binary path or URL); agents on any worker can call it.",
                trailing: has_relays.then(|| rsx! {
                    Button {
                        variant: ButtonVariant::Default,
                        icon: "add",
                        "data-testid": "mcp-add-btn",
                        onclick: move |_| editing.set(true),
                        "Add relay"
                    }
                }),
                if editor_open {
                    RelayEditor {
                        on_save,
                        on_cancel: move |_| editing.set(false),
                    }
                }
                if let Some(message) = view().delete_error.clone() {
                    p { class: "md-body-s", style: "color: var(--md-sys-color-error);", {message} }
                }
                if let Some(message) = view().load_error.clone() {
                    div {
                        style: "display: flex; align-items: center; gap: var(--md-space-2);",
                        "data-testid": "mcp-load-err",
                        Icon { name: "error", style: "color: var(--md-sys-color-error);" }
                        span { class: "md-body-m", style: "color: var(--md-sys-color-error);",
                            "Failed to load relays: {message}"
                        }
                        Button {
                            variant: ButtonVariant::Ghost,
                            "data-testid": "mcp-reload-btn",
                            onclick: on_reload,
                            "Retry"
                        }
                    }
                }
            }
            if has_relays {
                Card {
                    List { contained: true,
                        for relay in view().relays.to_vec() {
                            RelayRow { relay, view, confirming, pump: row_pump.clone() }
                        }
                    }
                }
            } else {
                Card {
                    EmptyState {
                        icon: "extension",
                        title: "No MCP relays configured",
                        supporting: "Add a relay to make MCP-served tools available to every agent on every machine.",
                        action: (!editor_open).then(|| rsx! {
                            Button {
                                variant: ButtonVariant::Default,
                                icon: "add",
                                onclick: move |_| editing.set(true),
                                "Add relay"
                            }
                        }),
                    }
                }
            }
        }
    }
}

/// One relay row, with the two-step removal v2 gated behind a confirm.
#[component]
fn RelayRow(
    relay: McpRelay,
    view: Signal<McpView>,
    confirming: Signal<Option<String>>,
    pump: Pump,
) -> Element {
    let id = relay.id.to_string();
    let kind = relay.kind.as_str().to_owned();
    let target = relay
        .config
        .get("command")
        .or_else(|| relay.config.get("url"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let is_confirming = confirming() == Some(id.clone());
    // Each answer is built once: an `rsx!` attribute body is a block, and a
    // block that clones per attribute moves the same `id` once per attribute
    // it appears in.
    let row_id = id.clone();
    let on_remove = move |_event: MouseEvent| confirming.set(Some(row_id.clone()));
    let on_confirm_remove = {
        let row_pump = pump.clone();
        let row_id = id.clone();
        move |_event: MouseEvent| {
            remove_relay(row_pump.clone(), row_id.clone(), view);
            confirming.set(None);
        }
    };
    let on_cancel_remove = move |_event: MouseEvent| confirming.set(None);
    rsx! {
        ListRow {
            test_id: Some(format!("mcp-relay-row-{id}")),
            leading_icon: Some(if kind == "sse" { "cloud".to_owned() } else { "terminal".to_owned() }),
            headline: rsx! {
                span { style: "display: flex; align-items: center; gap: var(--md-space-2);",
                    {relay.label.clone()}
                    span { class: "md-label-s", style: "padding: 2px 8px; border-radius: var(--md-shape-full); background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); text-transform: uppercase;", {kind.clone()} }
                }
            },
            support: (!target.is_empty()).then(|| rsx! { span { style: "font-family: var(--font-mono);", {target} } }),
            trailing: rsx! {
                if is_confirming {
                    Button {
                        variant: ButtonVariant::Destructive,
                        "data-testid": format!("mcp-confirm-delete-{id}"),
                        onclick: on_confirm_remove,
                        "Confirm"
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        "data-testid": format!("mcp-cancel-delete-{id}"),
                        onclick: on_cancel_remove,
                        "Cancel"
                    }
                } else {
                    Button {
                        variant: ButtonVariant::Destructive,
                        icon: "delete_outline",
                        "data-testid": format!("mcp-delete-{id}"),
                        onclick: on_remove,
                        "Remove"
                    }
                }
            },
        }
    }
}

/// The inline create form: a name, a kind, and the target that kind implies.
#[component]
fn RelayEditor(
    on_save: EventHandler<(String, String, String)>,
    on_cancel: EventHandler<()>,
) -> Element {
    let mut label = use_signal(String::new);
    let mut kind = use_signal(|| "stdio".to_owned());
    let mut target = use_signal(String::new);
    let target_label = if kind() == "sse" {
        "Server URL"
    } else {
        "Binary path"
    };
    let target_placeholder = if kind() == "sse" {
        "https://mcp.example.com/sse"
    } else {
        "/usr/local/bin/mcp-server"
    };
    rsx! {
        div {
            class: "settings-mcp-editor",
            style: "display: flex; flex-direction: column; gap: var(--md-space-2); padding: var(--md-space-3); border-radius: var(--md-shape-sm); border: 1px solid var(--md-sys-color-outline-variant); background: var(--md-elev-1);",
            "data-testid": "mcp-relay-editor",
            TextField {
                test_id: "mcp-editor-label",
                label: "Relay name",
                placeholder: "e.g. filesystem",
                value: label(),
                on_input: move |value| label.set(value),
            }
            Select {
                test_id: "mcp-editor-kind",
                label: "Kind",
                value: kind(),
                on_change: move |value| kind.set(value),
                options: vec![
                    SelectOption::new("stdio", "stdio (binary path)"),
                    SelectOption::new("sse", "sse (URL)"),
                ],
            }
            TextField {
                test_id: "mcp-editor-target",
                label: target_label,
                placeholder: target_placeholder,
                value: target(),
                on_input: move |value| target.set(value),
            }
            div { style: "display: flex; gap: var(--md-space-2);",
                Button {
                    variant: ButtonVariant::Default,
                    "data-testid": "mcp-editor-save",
                    disabled: label().trim().is_empty() || target().trim().is_empty(),
                    onclick: move |_| {
                        on_save.call((
                            label().trim().to_owned(),
                            kind().clone(),
                            relay_config_json(&kind(), target().trim()),
                        ));
                    },
                    "Add relay"
                }
                Button {
                    variant: ButtonVariant::Ghost,
                    "data-testid": "mcp-editor-cancel",
                    onclick: move |_| on_cancel.call(()),
                    "Cancel"
                }
            }
        }
    }
}

/// A relay's free-form config: `stdio` names a binary, `sse` a URL.
fn relay_config_json(kind: &str, target: &str) -> String {
    let key = if kind == "sse" { "url" } else { "command" };
    serde_json::json!({ key: target }).to_string()
}

/// Re-read the registry. A refusal is shown, never swallowed.
fn reload(pump: Pump, view: Signal<McpView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        match pump.rpc().call(&ListMcpRelays).await {
            Ok(mut rows) => {
                rows.sort_by_key(|relay| relay.created_at_ms);
                view.write().relays = rows;
                view.write().load_error = None;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "mcp relay list refused");
                view.write().load_error = Some(error.to_string());
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, view);
}

/// Register one relay, then re-read what the coordinator kept.
fn create_relay(
    pump: Pump,
    label: String,
    kind: String,
    config_json: String,
    view: Signal<McpView>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let request = CreateMcpRelay {
            label,
            kind,
            config_json,
        };
        if let Err(error) = pump.rpc().call(&request).await {
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                message: format!("Add failed: {error}"),
            }));
            return;
        }
        reload(pump.clone(), view);
        pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
            message: "Relay added".to_owned(),
        }));
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, label, kind, config_json, view);
}

/// Drop one relay, and say so when the coordinator will not.
fn remove_relay(pump: Pump, id: String, view: Signal<McpView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        let request = DeleteMcpRelay { id: id.clone() };
        match pump.rpc().call(&request).await {
            Ok(true) => {}
            Ok(false) => {
                let message = "the coordinator has no such relay".to_owned();
                tracing::warn!(target: "settings", %message, "relay delete refused");
                view.write().delete_error = Some(message.clone());
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("Delete failed: {message}"),
                }));
                return;
            }
            Err(error) => {
                view.write().delete_error = Some(error.to_string());
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("Delete failed: {error}"),
                }));
                return;
            }
        }
        view.write().delete_error = None;
        view.write()
            .relays
            .retain(|relay| relay.id.to_string() != id);
        pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
            message: "Relay removed".to_owned(),
        }));
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, id, view);
}
