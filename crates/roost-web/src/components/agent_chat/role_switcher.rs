//! The model panel the composer's model button opens: every model role this
//! chat runs (Default, Fast, Thinking, Architect…) with the model leading it,
//! what each role is for, and an editor to change a role's model and thinking
//! level in place. An edit saves to all folders, or to this chat's folder
//! only; a role this folder already overrides opens on the folder. Mounted by
//! `model_controls`; reads and writes through `settings::agent_roles`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ConversationSummary, ModelsCatalog};

use super::agent_alert::ERROR_TEXT_CLASS;
use super::model_controls::{Configure, configure};
use super::model_picker::ModelPicker;
use super::role_presets::RolePresets;
use super::role_switch::{
    RoleLine, initial_scope, pin_hides, role_lines, thinking_choices, with_role_leader,
    with_role_thinking,
};
use super::role_thinking::RoleThinking;
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
    let mut show_purpose = use_signal(|| false);
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
        (Some(opened), Some(catalog)) => {
            let role = opened.role;
            let active = scope();
            let on_folder = active == folder;
            // Re-read from the settings each render, so a save shows at once.
            let line = role_lines(
                &current,
                &active,
                Some(catalog),
                conversation.running_role(),
            )
            .into_iter()
            .find(|line| line.role == role)
            .unwrap_or(opened);
            let options = role_options(catalog, role);
            let clear_pin =
                conversation.model.is_some() && pin_hides(role, conversation.running_role());
            let conversation_id = conversation.id.clone();
            let choose_pump = pump.clone();
            let reset_pump = pump.clone();
            let reset_scope = folder.clone();
            let choose_current = current.clone();
            let reset_current = current.clone();
            let kept_thinking = line.thinking.clone();
            let levels = catalog.thinking_levels.clone();
            let choose_catalog = catalog.clone();
            let thinking_scope = active.clone();
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
                p { class: "agent-chat__roles-purpose", "{line.purpose}" }
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
                RoleThinking {
                    line: line.clone(),
                    catalog: catalog.clone(),
                    scope: thinking_scope,
                    pump: pump.clone(),
                    settings,
                    error,
                }
                ModelPicker {
                    options,
                    selected: line.selector.clone(),
                    placeholder: "Search models",
                    test_id: "agent-roles-picker",
                    autofocus: !compact,
                    on_choose: move |selector: String| {
                        let mut updated = with_role_leader(&choose_current, &active, role, &selector);
                        // The new model keeps the role's thinking level when it accepts it.
                        if let Some(level) = kept_thinking.as_deref()
                            && thinking_choices(&choose_catalog, &selector).iter().any(|choice| choice == level)
                        {
                            updated = with_role_thinking(&updated, &active, role, Some(level), &levels);
                        }
                        save_settings(choose_pump.clone(), updated, settings, error);
                        // A session pin runs ahead of this role; clear it so this chat follows.
                        if clear_pin {
                            configure(choose_pump.clone(), conversation_id.clone(), Configure::Model(None));
                        }
                        editing.set(None);
                    },
                }
            }
        }
        _ => {
            let lines = role_lines(
                &current,
                &folder,
                catalog.as_ref(),
                conversation.running_role(),
            );
            let settings_pane = crate::routes::settings_pane_href("models");
            rsx! {
                div { class: "agent-chat__roles-head",
                    span { class: "agent-chat__roles-title", "Models" }
                    IconButton {
                        icon: "info",
                        label: if show_purpose() { "Hide what each role is for" } else { "Show what each role is for" },
                        title: "What each role is for",
                        variant: if show_purpose() { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                        size: IconButtonSize::IconSm,
                        "data-testid": "agent-roles-help",
                        onclick: move |_| show_purpose.toggle(),
                    }
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
                RolePresets {
                    conversation: conversation.clone(),
                    settings,
                    error,
                    pump: pump.clone(),
                }
                List {
                    for line in lines {
                        ListRow {
                            key: "{line.role}",
                            test_id: Some(format!("agent-roles-{}", line.role)),
                            class: Some(if show_purpose() { "agent-chat__roles-row agent-chat__roles-row--explained" } else { "agent-chat__roles-row" }.to_owned()),
                            dense: true,
                            headline: rsx! { span { "{line.title}" } },
                            support: Some(rsx! {
                                span { "{line.model}" }
                                if show_purpose() {
                                    span { class: "agent-chat__roles-purpose", "{line.purpose}" }
                                }
                            }),
                            trailing: (line.this_chat || line.folder_override).then(|| rsx! {
                                if line.this_chat {
                                    Chip { label: "This chat".to_owned(), small: true, selected: Some(true),
                                        title: Some("This chat's own turns run on this role".to_owned()),
                                    }
                                }
                                if line.folder_override {
                                    Chip { label: "This folder".to_owned(), small: true }
                                }
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
