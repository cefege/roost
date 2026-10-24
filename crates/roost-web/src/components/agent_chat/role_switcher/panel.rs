//! Editable model-role settings inside the composer's role popover.
//! RoleSwitcher owns opening and hydration; this panel owns role and scope edits.
//! Saves use the existing settings RPCs and preserve conversation pin precedence.

use dioxus::prelude::*;
use libero::components::{ActionIcon, Badge, Button, Chip, Flex, Pictogram, Text};
use libero::sx::sx;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ConversationSummary, ModelsCatalog};

use super::super::model_controls::{Configure, configure};
use super::super::model_picker::ModelPicker;
use super::super::role_switch::{
    RoleLine, initial_scope, pin_hides, role_lines, thinking_choices, with_role_leader,
    with_role_thinking,
};
use super::super::role_thinking::RoleThinking;
use crate::components::agent_chat::role_presets::RolePresets;
use crate::components::layout::window_size::use_is_compact;
use crate::components::settings::agent_roles::{role_options, save_settings};
use crate::components::settings::model_scope::{ModelScope, with_role_value};
use crate::pump::Pump;
use crate::router_state::use_navigate;

#[component]
pub(super) fn RolePanel(
    editing: Signal<Option<RoleLine>>,
    on_close: EventHandler<()>,
    on_layout_change: EventHandler<()>,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let mut editing = editing;
    let compact = use_is_compact();
    let navigate = use_navigate();
    let mut show_purpose = use_signal(|| false);
    let mut scope = use_signal(|| ModelScope::Global);
    let panel_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("md"));
    let section_row_sx = use_hook(|| sx().display("flex").align_items("center").gap("sm"));
    let wrapped_row_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let purpose_sx = use_hook(|| sx().margin("0"));
    let role_button_sx = use_hook(|| {
        sx().width("100%")
            .justify_content("start")
            .text_align("left")
            .padding_block("sm")
    });
    let role_content_sx = use_hook(|| {
        sx().width("100%")
            .min_width("0")
            .display("flex")
            .flex_direction("column")
            .align_items("start")
            .gap("xs")
    });
    let role_badges_sx = use_hook(|| sx().display("flex").gap("xs"));
    use_effect(move || {
        let _purpose = show_purpose();
        let _scope = scope.read();
        on_layout_change.call(());
    });
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
                Flex { sx: section_row_sx, direction: "row", align: "center", gap: "sm",
                    ActionIcon {
                        icon: pictogram_icons_lucide::arrow_left::outlined,
                        aria_label: "All roles",
                        title: "All roles",
                        size: "sm",
                        "data-testid": "agent-roles-back",
                        onclick: move |_| editing.set(None),
                    }
                    Text { size: "sm", "{line.title}" }
                }
                Text { sx: purpose_sx, size: "sm", color: "muted", "{line.purpose}" }
                // Filter chips: the chosen one carries the check.
                Flex { sx: wrapped_row_sx, direction: "row", align: "center", gap: "sm", role: "group", "aria-label": "Applies to",
                    Chip {
                        checked: !on_folder,
                        icon: (!on_folder).then(|| rsx! { Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "1rem", height: "1rem" } }),
                        "data-testid": "agent-roles-scope-global",
                        onclick: move |_| scope.set(ModelScope::Global),
                        "All folders"
                    }
                    Chip {
                        checked: on_folder,
                        icon: on_folder.then(|| rsx! { Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "1rem", height: "1rem" } }),
                        title: conversation.cwd.clone(),
                        "data-testid": "agent-roles-scope-folder",
                        onclick: {
                            let folder = folder.clone();
                            move |_| scope.set(folder.clone())
                        },
                        "This folder"
                    }
                    if on_folder && line.folder_override {
                        Button {
                            variant: "outlined",
                            size: "sm",
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
                Flex { sx: section_row_sx, direction: "row", align: "center", gap: "sm",
                    Text { size: "sm", "Models" }
                    ActionIcon {
                        icon: pictogram_icons_lucide::info::outlined,
                        aria_label: if show_purpose() { "Hide what each role is for" } else { "Show what each role is for" },
                        title: "What each role is for",
                        size: "sm",
                        "data-testid": "agent-roles-help",
                        onclick: move |_| show_purpose.toggle(),
                    }
                    Button {
                        variant: "standard",
                        size: "sm",
                        "data-testid": "agent-roles-settings",
                        title: "Fallback chains, presets and accounts",
                        onclick: move |_| {
                            on_close.call(());
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
                Flex { direction: "column",
                    for line in lines {
                        {
                            let line_for_click = line.clone();
                            let start = initial_scope(&current, &folder, line.role);
                            rsx! {
                                Button {
                                    key: "{line.role}",
                                    variant: "standard",
                                    sx: role_button_sx.clone(),
                                    "data-testid": format!("agent-roles-{}", line.role),
                                    onclick: move |_| {
                                        scope.set(start.clone());
                                        editing.set(Some(line_for_click.clone()));
                                    },
                                    Flex { sx: role_content_sx.clone(), direction: "column", align: "start", gap: "xs",
                                        Text { size: "sm", "{line.title}" }
                                        Text { size: "sm", "{line.model}" }
                                        if show_purpose() {
                                            Text { size: "sm", color: "muted", "{line.purpose}" }
                                        }
                                        if line.this_chat || line.folder_override {
                                            Flex { sx: role_badges_sx.clone(), direction: "row", gap: "xs",
                                                if line.this_chat {
                                                    Badge { variant: "tonal", "This chat" }
                                                }
                                                if line.folder_override {
                                                    Badge { variant: "outlined", "This folder" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    rsx! {
        Flex { sx: panel_sx, direction: "column", gap: "md",
            {body}
            if !error().is_empty() {
                Text { size: "sm", color: "error", role: "alert", "{error}" }
            }
        }
    }
}
