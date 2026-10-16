//! The model panel the composer's model button opens: every model role this
//! chat runs (default, quick tasks, review, planning, delegated work…) with
//! the model leading it, and a picker to change one in place. An edit saves
//! to all folders, or to this chat's folder only; a role this folder already
//! overrides opens on the folder. Mounted by `model_controls`; reads and
//! writes through `settings::agent_roles`, the one settings loader and writer.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ConversationSummary, ModelsCatalog};

use super::agent_alert::ERROR_TEXT_CLASS;
use super::model_controls::{Configure, configure};
use super::model_picker::ModelPicker;
use super::role_switch::{RoleLine, initial_scope, role_lines, with_role_leader};
use crate::components::context_menu::use_floating_menu_dismiss;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Chip, IconButton, IconButtonSize, List, ListRow, Surface,
};
use crate::components::settings::agent_roles::{load_settings, role_options, save_settings};
use crate::components::settings::model_scope::{ModelScope, with_role_value};
use crate::pump::Pump;
use crate::router_state::use_navigate;

/// Holds the settings for the panel, fresh each time it opens, so a save
/// elsewhere never shows stale here.
#[component]
pub(super) fn RoleSwitcher(
    open: Signal<bool>,
    trigger_id: String,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
) -> Element {
    let settings = use_signal(AgentSettingsView::default);
    let error = use_signal(String::new);
    let load_pump = pump.clone();
    use_effect(move || {
        if open() {
            load_settings(load_pump.clone(), settings, error);
        }
    });
    if !open() {
        return rsx! {};
    }
    rsx! {
        RolePanel { open, trigger_id, conversation, catalog, pump, settings, error }
    }
}

#[component]
fn RolePanel(
    open: Signal<bool>,
    trigger_id: String,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let mut open = open;
    let compact = use_is_compact();
    let navigate = use_navigate();
    let mut editing = use_signal(|| None::<RoleLine>);
    let mut scope = use_signal(|| ModelScope::Global);
    let panel_id = format!("{trigger_id}-roles");
    use_floating_menu_dismiss(
        EventHandler::new(move |()| open.set(false)),
        // Escape steps back out of a picker before it closes the panel.
        Some(EventHandler::new(move |()| {
            if editing.peek().is_some() {
                editing.set(None);
            } else {
                open.set(false);
            }
        })),
        vec![trigger_id.clone(), panel_id.clone()],
    );
    let folder = ModelScope::Folder {
        worker_fp: conversation.worker_fp.clone(),
        cwd: conversation.cwd.clone(),
    };
    let current = settings();
    let body = match (editing(), catalog.as_ref()) {
        (Some(line), Some(catalog)) => {
            let role = line.role;
            let active = scope();
            let on_folder = active == folder;
            let options = role_options(catalog, role);
            let pinned = conversation.model.is_some();
            let conversation_id = conversation.id.clone();
            let choose_pump = pump.clone();
            let reset_pump = pump.clone();
            let reset_scope = folder.clone();
            let choose_current = current.clone();
            let reset_current = current.clone();
            rsx! {
                div { class: "agent-chat__roles-head",
                    IconButton {
                        icon: "arrow_back",
                        label: "All roles",
                        title: "All roles",
                        variant: ButtonVariant::Ghost,
                        size: IconButtonSize::IconSm,
                        "data-testid": "agent-roles-back",
                        onclick: move |_| editing.set(None),
                    }
                    span { class: "agent-chat__roles-title", "{line.title}" }
                }
                // Material filter chips: the chosen one carries the check.
                div { class: "agent-chat__roles-scope", role: "group", "aria-label": "Applies to",
                    Chip { label: "All folders".to_owned(), selected: Some(!on_folder),
                        icon: (!on_folder).then(|| "check".to_owned()),
                        test_id: Some("agent-roles-scope-global".to_owned()),
                        onclick: Some(EventHandler::new(move |_| scope.set(ModelScope::Global))),
                    }
                    Chip { label: "This folder".to_owned(), selected: Some(on_folder),
                        icon: on_folder.then(|| "check".to_owned()),
                        title: Some(conversation.cwd.clone()),
                        test_id: Some("agent-roles-scope-folder".to_owned()),
                        onclick: Some(EventHandler::new({
                            let folder = folder.clone();
                            move |_| scope.set(folder.clone())
                        })),
                    }
                    if on_folder && line.folder_override {
                        Button {
                            variant: ButtonVariant::Link,
                            size: ButtonSize::Sm,
                            "data-testid": "agent-roles-use-all-folders",
                            onclick: move |_| {
                                let updated = with_role_value(&reset_current, &reset_scope, role, None);
                                save_settings(reset_pump.clone(), updated, settings, error);
                                editing.set(None);
                            },
                            "Use all-folders model"
                        }
                    }
                }
                ModelPicker {
                    options,
                    selected: line.selector.clone(),
                    placeholder: "Search models",
                    test_id: "agent-roles-picker",
                    autofocus: !compact,
                    on_choose: move |selector: String| {
                        let updated = with_role_leader(&choose_current, &active, role, &selector);
                        save_settings(choose_pump.clone(), updated, settings, error);
                        // The default role is what this chat runs; a pin would hide the change.
                        if role == "default" && pinned {
                            configure(choose_pump.clone(), conversation_id.clone(), Configure::Model(None));
                        }
                        editing.set(None);
                    },
                }
            }
        }
        _ => {
            let lines = role_lines(&current, &folder, catalog.as_ref());
            let settings_pane = crate::routes::settings_pane_href("models");
            rsx! {
                div { class: "agent-chat__roles-head",
                    span { class: "agent-chat__roles-title", "Models" }
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        "data-testid": "agent-roles-settings",
                        title: "Fallback chains, presets and accounts",
                        onclick: move |_| {
                            open.set(false);
                            navigate.call(settings_pane.clone());
                        },
                        "Settings"
                    }
                }
                List {
                    for line in lines {
                        ListRow {
                            key: "{line.role}",
                            test_id: Some(format!("agent-roles-{}", line.role)),
                            class: Some("agent-chat__roles-row".to_owned()),
                            dense: true,
                            headline: rsx! { span { "{line.title}" } },
                            support: Some(rsx! { span { "{line.model}" } }),
                            trailing: line.folder_override.then(|| rsx! {
                                Chip { label: "This folder".to_owned(), small: true }
                            }),
                            onclick: EventHandler::new({
                                let line = line.clone();
                                let start = initial_scope(&current, &folder, line.role);
                                move |_| {
                                    scope.set(start.clone());
                                    editing.set(Some(line.clone()));
                                }
                            }),
                        }
                    }
                }
            }
        }
    };
    rsx! {
        div { class: "agent-chat__roles", id: "{panel_id}", "data-testid": "agent-roles",
            Surface { class: "agent-chat__roles-panel", level: 1, elevation: 2, border: true, pad: Some(2),
                {body}
                if !error().is_empty() {
                    p { class: ERROR_TEXT_CLASS, role: "alert", "{error}" }
                }
            }
        }
    }
}
