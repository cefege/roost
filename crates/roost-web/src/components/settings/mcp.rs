//! Settings → Agents → MCP: the relay registry.
//!
//! Ports `apps/web/src/components/Settings/McpPane.tsx` and the inline
//! `apps/web/src/components/agents/McpRelayEditor.tsx` it opens. Depends on
//! `roost-client-core`'s `McpList`/`McpCreate`/`McpDelete` calls and on the
//! `libero` primitives.
//!
//! The rendered list is seeded from the store's `mcp_relays` projection — the
//! same rows v2's root store held — and re-read from `McpList`, so a
//! coordinator that REFUSES the read shows its refusal beside the projection
//! instead of an empty pane that looks like "you have no relays".

use dioxus::prelude::*;
use roost_protocol::wire::McpRelay;

use crate::pump::{Pump, use_store};
use libero::components::{
    Badge, Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Paper, Pictogram, Text,
    TextField, use_combobox,
};
#[path = "mcp_operations.rs"]
mod operations;
use operations::{create_relay, reload, remove_relay};

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
pub(super) struct McpView {
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
    let semibold_text_sx = use_hook(|| libero::sx::sx().font_weight("600"));
    let paper_sx = use_hook(|| libero::sx::sx().padding("md"));

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
        Flex { direction: "column", gap: "md", "data-testid": "settings-mcp-pane",
            Paper { sx: paper_sx.clone(),
                Flex { direction: "column", gap: "sm",
                    Flex { direction: "row", align: "center", justify: "space-between", gap: "sm",
                        Text { size: "lg", sx: semibold_text_sx.clone(), "MCP relays" }
                        if has_relays {
                            Button {
                                variant: "filled",
                                icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::plus::outlined } }),
                                "data-testid": "mcp-add-btn",
                                onclick: move |_| editing.set(true),
                                "Add relay"
                            }
                        }
                    }
                    Text { size: "sm", color: "muted",
                        "Bridge agents to external tools and data sources. Each relay points at an MCP server (binary path or URL); agents on any worker can call it."
                    }
                    if editor_open {
                        RelayEditor { on_save, on_cancel: move |_| editing.set(false) }
                    }
                    if let Some(message) = view().delete_error.clone() {
                        Text { size: "sm", color: "error", role: "alert", "{message}" }
                    }
                    if let Some(message) = view().load_error.clone() {
                        Flex { direction: "row", align: "center", gap: "sm", "data-testid": "mcp-load-err",
                            Pictogram { icon: pictogram_icons_lucide::circle_alert::outlined }
                            Text { size: "sm", color: "error", role: "alert", "Failed to load relays: {message}" }
                            Button { variant: "standard", "data-testid": "mcp-reload-btn", onclick: on_reload, "Retry" }
                        }
                    }
                }
            }
            Paper { sx: paper_sx,
                if has_relays {
                    Flex { direction: "column", gap: "xs",
                        for relay in view().relays.to_vec() {
                            RelayRow { relay, view, confirming, pump: row_pump.clone() }
                        }
                    }
                } else {
                    Flex { direction: "column", align: "center", gap: "xs",
                        Pictogram { icon: pictogram_icons_lucide::puzzle::outlined }
                        Text { size: "md", sx: semibold_text_sx.clone(), "No MCP relays configured" }
                        Text { size: "sm", color: "muted", "Add a relay to make MCP-served tools available to every agent on every machine." }
                        if !editor_open {
                            Button {
                                variant: "filled",
                                icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::plus::outlined } }),
                                onclick: move |_| editing.set(true),
                                "Add relay"
                            }
                        }
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
    let medium_text_sx = use_hook(|| libero::sx::sx().font_weight("500"));
    let kind_sx = use_hook(|| libero::sx::sx().with("text-transform", "uppercase"));
    let target_sx = use_hook(|| libero::sx::sx().font_family("monospace"));
    rsx! {
        Flex { direction: "row", align: "center", justify: "space-between", gap: "sm",
            "data-testid": format!("mcp-relay-row-{id}"),
            Flex { direction: "row", align: "center", gap: "sm",
                Pictogram { icon: if kind == "sse" { pictogram_icons_lucide::cloud::outlined } else { pictogram_icons_lucide::terminal::outlined } }
                Flex { direction: "column", gap: "xs",
                    Flex { direction: "row", align: "center", gap: "xs",
                        Text { size: "sm", sx: medium_text_sx.clone(), "{relay.label}" }
                        Badge { variant: "outlined", sx: kind_sx.clone(), "{kind}" }
                    }
                    if !target.is_empty() {
                        Text { size: "xs", color: "muted", sx: target_sx.clone(), "{target}" }
                    }
                }
            }
            Flex { direction: "row", align: "center", gap: "xs",
                if is_confirming {
                    Button { variant: "outlined", color: "error", "data-testid": format!("mcp-confirm-delete-{id}"), onclick: on_confirm_remove, "Confirm" }
                    Button { variant: "standard", "data-testid": format!("mcp-cancel-delete-{id}"), onclick: on_cancel_remove, "Cancel" }
                } else {
                    Button { variant: "outlined", color: "error", icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::trash_2::outlined } }),
                        "data-testid": format!("mcp-delete-{id}"), onclick: on_remove, "Remove"
                    }
                }
            }
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
    let kind_select = use_combobox();
    let kind_options = vec![
        ("stdio".to_owned(), "stdio (binary path)".to_owned()),
        ("sse".to_owned(), "sse (URL)".to_owned()),
    ];

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
    let editor_sx = use_hook(|| libero::sx::sx().padding("sm"));
    rsx! {
        Paper { sx: editor_sx, "data-testid": "mcp-relay-editor",
            Flex { direction: "column", gap: "sm",
                TextField {
                    value: Some(label()), oninput: move |value| label.set(value),
                    label: "Relay name", placeholder: "e.g. filesystem", "data-testid": "mcp-editor-label",
                }
                Flex { direction: "column", gap: "xs",
                    Text { size: "sm", "Kind" }
                    Combobox {
                        state: kind_select,
                        select_only: true,
                        aria_label: "Kind",
                        selected_index: kind_options.iter().position(|(value, _)| *value == kind()),
                        option_label: |option: (String, String)| option.1.clone(),
                        options: kind_options.clone(),
                        option: {
                            let current = kind();
                            move |row: ComboboxOptionArgs<(String, String)>| {
                                let value = row.value.0.clone();
                                let selected_kind = current.clone();
                                rsx! {
                                    ComboboxOption {
                                        selected: selected_kind == row.value.0,
                                        onpick: move |_| {
                                            kind.set(value.clone());
                                            kind_select.close();
                                        },
                                        "{row.value.1}"
                                    }
                                }
                            }
                        },
                        Button {
                            attributes: kind_select.a11y_attributes(),
                            onclick: move |_| kind_select.toggle(),
                            onblur: move |_| kind_select.close(),
                            "data-testid": "mcp-editor-kind",
                            "{kind_options.iter().find(|(value, _)| *value == kind()).map(|(_, label)| label.as_str()).unwrap_or_default()}"
                        }
                    }
                }
                TextField {
                    value: Some(target()), oninput: move |value| target.set(value),
                    label: target_label, placeholder: target_placeholder, "data-testid": "mcp-editor-target",
                }
                Flex { direction: "row", gap: "xs",
                    Button {
                        variant: "filled",
                        "data-testid": "mcp-editor-save",
                        disabled: label().trim().is_empty() || target().trim().is_empty(),
                        onclick: move |_| on_save.call((
                            label().trim().to_owned(),
                            kind().clone(),
                            relay_config_json(&kind(), target().trim()),
                        )),
                        "Add relay"
                    }
                    Button { variant: "standard", "data-testid": "mcp-editor-cancel", onclick: move |_| on_cancel.call(()), "Cancel" }
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
