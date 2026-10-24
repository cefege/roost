//! One role's row in the Models pane: its source chip, the reset action for a
//! folder override, and the chain editor. `agent_roles` renders the all-folders
//! role and each task-override role through `role_row`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use super::{role_options, save_settings};
use crate::components::settings::model_chain::{join_chain, parse_chain};
use crate::components::settings::model_chain_editor::ModelChainEditor;
use crate::components::settings::model_scope::{
    ModelScope, RoleSource, role_source, role_value, with_role_value,
};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardAction, CardContent, CardDescription, CardHeader,
    CardTitle, Chip,
};
use crate::pump::Pump;

pub(super) struct RoleRow {
    pub(super) role: &'static str,
    pub(super) title: &'static str,
    pub(super) purpose: &'static str,
    pub(super) inherits: &'static str,
}

/// What a role row reads and writes, shared by every row in the pane.
pub(super) struct RoleContext<'a> {
    pub(super) current: &'a AgentSettingsView,
    pub(super) scope: &'a ModelScope,
    pub(super) catalog: &'a ModelsCatalog,
    pub(super) pump: Pump,
    pub(super) settings: Signal<AgentSettingsView>,
    pub(super) error: Signal<String>,
}

/// The source chip's text and tooltip. In all-folders scope a role is only ever
/// set or not; in a folder it can also come from the all-folders chain.
fn source_chip(source: RoleSource, is_folder: bool) -> (String, String) {
    match (source, is_folder) {
        (RoleSource::Folder, _) => (
            "This folder".to_owned(),
            "This folder's own chain runs".to_owned(),
        ),
        (RoleSource::Global, true) => (
            "All folders".to_owned(),
            "Inherited from the all-folders chain".to_owned(),
        ),
        (RoleSource::Global, false) => ("Set".to_owned(), "Saved for all folders".to_owned()),
        (RoleSource::Unset, _) => (
            "Not set".to_owned(),
            "Nothing saved; the inherited choice runs".to_owned(),
        ),
    }
}

pub(super) fn role_row(row: RoleRow, context: &RoleContext<'_>) -> Element {
    let RoleRow {
        role,
        title,
        purpose,
        inherits,
    } = row;
    let chain = parse_chain(&role_value(context.current, context.scope, role).unwrap_or_default());
    let source = role_source(context.current, context.scope, role);
    let is_folder = matches!(context.scope, ModelScope::Folder { .. });
    let (source_text, source_title) = source_chip(source, is_folder);
    let options = role_options(context.catalog, role);
    let thinking_levels = context.catalog.thinking_levels.clone();
    let settings = context.settings;
    let error = context.error;
    let edit_current = context.current.clone();
    let edit_scope = context.scope.clone();
    let edit_pump = context.pump.clone();
    let reset_current = context.current.clone();
    let reset_scope = context.scope.clone();
    let reset_pump = context.pump.clone();
    let show_reset = is_folder && source == RoleSource::Folder;
    rsx! {
        Card {
            key: "{role}",
            "data-testid": format!("agent-role-{role}"),
            CardHeader {
                CardTitle { {title} }
                CardDescription { {purpose} }
                CardAction {
                    div { class: "flex flex-wrap items-center gap-2",
                        Chip {
                            label: source_text,
                            title: Some(source_title),
                            test_id: Some(format!("agent-role-{role}-source")),
                        }
                        if show_reset {
                            Button {
                                variant: ButtonVariant::Ghost,
                                size: ButtonSize::Sm,
                                "data-testid": "agent-role-{role}-use-all-folders",
                                onclick: move |_| {
                                    let updated = with_role_value(&reset_current, &reset_scope, role, None);
                                    save_settings(reset_pump.clone(), updated, settings, error);
                                },
                                "Use all-folders chain"
                            }
                        }
                    }
                }
            }
            CardContent {
                ModelChainEditor {
                    role_title: title.to_owned(),
                    chain,
                    options,
                    thinking_levels,
                    inherited_label: inherits.to_owned(),
                    test_id: format!("agent-role-{role}-chain"),
                    on_change: move |next: Vec<String>| {
                        let updated = with_role_value(&edit_current, &edit_scope, role, Some(join_chain(&next)));
                        save_settings(edit_pump.clone(), updated, settings, error);
                    },
                }
            }
        }
    }
}
