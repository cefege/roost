//! The Roles view's markup: the tab filter, the preset chips, the role list, the
//! inline model picker and the footer keys. Every event is forwarded to
//! `actions::RoleContext`; this file reads state and renders it. Mounted by
//! `model_hub` as the all-folders role editor.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ConversationSummary, ModelsCatalog};

use super::actions::{NAME_FIELD_ID, PickTarget, ROOT_ID, RoleAction, RoleContext, handle_key};
use super::edits::{active_preset_name, preset_names, split_entry};
use super::rows::{
    LeaderValue, RoleTab, RoleTone, RolesRow, build_rows, leader_value, role_inherits, role_tag,
};
use crate::components::agent_chat::model_controls::thinking_label;
use crate::components::agent_chat::model_picker::ModelPicker;
use crate::components::file_viewer::dom::focus_by_id;
use crate::components::settings::agent_roles::role_options;
use crate::pump::Pump;
use libero::components::{Button, Chip, Divider, Flex, Pictogram, Text, TextField};
use libero::sx::sx;

/// The footer's key legend: the keys this view runs.
const KEY_LEGEND: &str = "↑↓ rows · ⏎ pick · f fallback · x clear · t thinking · [ ] reorder · s save preset · p/P preset · alt+←/→ tab";

#[component]
pub fn HubRolesView(
    catalog: ModelsCatalog,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
    pump: Pump,
    conversation: ConversationSummary,
    focused: bool,
) -> Element {
    let tab = use_signal(|| RoleTab::All);
    let cursor = use_signal(|| 0_usize);
    let anchor = use_signal(|| "default".to_owned());
    let picking = use_signal(|| None::<PickTarget>);
    let naming = use_signal(|| false);
    let preset_name = use_signal(String::new);

    use_effect(use_reactive((&focused,), move |(focused,)| {
        if focused {
            focus_by_id(ROOT_ID);
        }
    }));
    use_effect(move || {
        if naming() {
            focus_by_id(NAME_FIELD_ID);
        }
    });

    let context = RoleContext {
        catalog: catalog.clone(),
        settings,
        error,
        pump,
        conversation_id: conversation.id.clone(),
        running: conversation.running_role(),
        pinned: conversation.model.is_some(),
        tab,
        cursor,
        anchor,
        picking,
        naming,
        preset_name,
    };

    let current = settings();
    let rows = build_rows(&current, tab());
    let cursor_row = cursor().min(rows.len().saturating_sub(1));
    let active = active_preset_name(&current);
    let names = preset_names(&current);
    let picker_target = picking();
    let picker_selected = picker_target
        .as_ref()
        .and_then(|target| target.current_selector(&current));
    let error_text = error();
    let typed = preset_name().trim().to_owned();
    let key_context = context.clone();
    let save_context = context.clone();
    let cancel_context = context.clone();
    let field_context = context.clone();
    let open_context = context.clone();
    let save_typed = typed.clone();
    let save_disabled = typed.is_empty();
    let root_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .flex("1")
            .min_height("0")
            .gap("sm")
    });
    let group_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let list_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .flex("1")
            .min_height("0")
            .overflow_y("auto")
    });
    let list_frame_sx = use_hook(|| sx().min_height("0").flex("1"));
    let fixed_sx = use_hook(|| sx().flex_shrink("0"));
    let footer_sx = use_hook(|| {
        sx().flex_shrink("0")
            .border_top("1px solid")
            .border_color("muted.4")
            .padding_top("sm")
    });

    rsx! {
        Flex {
            sx: root_sx,
            id: ROOT_ID,
            tabindex: "0",
            role: "region",
            "aria-label": "Model roles",
            onkeydown: move |event: KeyboardEvent| handle_key(&key_context, &event),
            Text { size: "sm", color: "muted", "Model roles — f adds a fallback, x clears a role" }
            if !error_text.is_empty() {
                Text { size: "sm", color: "error", role: "alert", "{error_text}" }
            }
            Flex { direction: "row", wrap: true, sx: group_sx.clone(), role: "group", "aria-label": "Role filter",
                for choice in RoleTab::ALL {
                    Chip {
                        key: "{choice.label()}",
                        checked: tab() == choice,
                        onclick: move |_| {
                            let mut tab_signal = tab;
                            tab_signal.set(choice);
                            let mut cursor_signal = cursor;
                            cursor_signal.set(0);
                        },
                        "{choice.label()}"
                    }
                }
            }
            Flex { direction: "row", wrap: true, sx: group_sx.clone(), role: "group", "aria-label": "Presets",
                for name in names {
                    PresetChip {
                        key: "{name}",
                        is_active: active.as_deref() == Some(name.as_str()),
                        name: name.clone(),
                        on_apply: {
                            let apply_context = context.clone();
                            move |name: String| apply_context.apply_named_preset(&name)
                        },
                    }
                }
                if !naming() {
                    Chip {
                        icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::plus::outlined } }),
                        "data-testid": "agent-hub-roles-preset-save",
                        title: "Save these roles as a preset",
                        onclick: move |_| open_context.run(RoleAction::OpenNaming),
                        "Save as…"
                    }
                }
            }
            if naming() {
                Flex { direction: "row", wrap: true, align: "end", sx: group_sx.clone(),
                    TextField {
                        value: preset_name(),
                        oninput: move |text: String| {
                            let mut field = preset_name;
                            field.set(text);
                        },
                        placeholder: "Preset name".to_owned(),
                        aria_label: "Preset name",
                        autocomplete: "off",
                        maxlength: 64,
                        "data-testid": NAME_FIELD_ID,
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Named(NamedKey::Enter) && !event.is_composing() {
                                event.prevent_default();
                                if !save_disabled {
                                    field_context.save_preset(&save_typed);
                                }
                            }
                        },
                    }
                    Button {
                        variant: "filled",
                        size: "sm",
                        disabled: save_disabled,
                        "data-testid": "agent-hub-roles-preset-save-confirm",
                        onclick: move |_| save_context.save_preset(&typed),
                        "Save"
                    }
                    Button {
                        variant: "standard",
                        size: "sm",
                        onclick: move |_| cancel_context.cancel_naming(),
                        "Cancel"
                    }
                }
            }
            Flex { sx: list_frame_sx,
                Flex { sx: list_sx, role: "listbox", "aria-label": "Roles", tabindex: "0", "aria-activedescendant": format!("agent-hub-roles-option-{cursor_row}"),
                    for (position, row) in rows.iter().cloned().enumerate() {
                        RoleRowLine {
                            key: "{position}",
                            position,
                            row,
                            selected: position == cursor_row,
                            catalog: catalog.clone(),
                            settings: current.clone(),
                            on_activate: {
                                let row_context = context.clone();
                                move |_| row_context.run(RoleAction::Activate(position))
                            },
                        }
                    }
                }
            }
            if let Some(target) = picker_target {
                Flex { sx: fixed_sx,
                    ModelPicker {
                        options: role_options(&catalog, target.role()),
                        selected: picker_selected,
                        on_choose: {
                            let picker_context = context.clone();
                            EventHandler::new(move |selector: String| {
                                picker_context.choose(&target, &selector);
                            })
                        },
                        placeholder: "Search models".to_owned(),
                        test_id: "agent-hub-roles-picker".to_owned(),
                    }
                }
            }
            Text { sx: footer_sx, size: "xs", color: "muted", "{KEY_LEGEND}" }
        }
    }
}

/// A saved or built-in preset chip. Clicking it applies the preset to all folders.
#[component]
fn PresetChip(is_active: bool, name: String, on_apply: EventHandler<String>) -> Element {
    let label = name.clone();
    let test_id = format!("agent-hub-roles-preset-{name}");
    rsx! {
        Chip {
            checked: is_active,
            icon: is_active.then(|| rsx! {
                Pictogram { icon: pictogram_icons_lucide::check::outlined }
            }),
            "data-testid": test_id,
            title: "Use {name} for every role, in all folders",
            onclick: move |_| on_apply.call(name.clone()),
            "{label}"
        }
    }
}

/// One row of the list: a role's tag and leader, a fallback, the separator, or
/// the add row. Clicks forward to `on_activate`.
#[component]
fn RoleRowLine(
    position: usize,
    row: RolesRow,
    selected: bool,
    catalog: ModelsCatalog,
    settings: AgentSettingsView,
    on_activate: EventHandler<()>,
) -> Element {
    let option_id = format!("agent-hub-roles-option-{position}");
    let aria_selected = if selected { "true" } else { "false" };
    let row_sx = use_hook(|| {
        sx().width("100%")
            .justify_content("flex-start")
            .text_align("left")
            .padding("sm")
    });
    let value_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    match row {
        RolesRow::Separator => rsx! { Divider {} },
        RolesRow::NewFallback => rsx! {
            Button {
                sx: row_sx.clone(),
                variant: if selected { "tonal" } else { "standard" },
                id: "{option_id}",
                role: "option",
                aria_selected,
                "data-testid": "{option_id}",
                onclick: move |_| on_activate.call(()),
                "+ New fallback…"
            }
        },
        RolesRow::Role { role } => {
            let (tag, tone) = role_tag(role);
            let value = leader_row_value(&catalog, &settings, role);
            let color = match tone {
                RoleTone::Ok => "success",
                RoleTone::Warn => "warning",
                RoleTone::Accent => "primary",
                RoleTone::Muted | RoleTone::Dim => "text-dimmed",
            };
            rsx! {
                Button {
                    sx: row_sx.clone(),
                    variant: if selected { "tonal" } else { "standard" },
                    id: "{option_id}",
                    role: "option",
                    aria_selected,
                    "data-testid": "{option_id}",
                    onclick: move |_| on_activate.call(()),
                    Flex { direction: "row", wrap: true, sx: value_sx,
                        Text { size: "xs", color, "{tag}" }
                        {value}
                    }
                }
            }
        }
        RolesRow::Fallback {
            index, selector, ..
        } => {
            let (base, level) = split_entry(&catalog, &selector);
            let (provider, model) = split_provider(base);
            let level_label = level.map(thinking_label);
            rsx! {
                Button {
                    sx: row_sx.clone(),
                    variant: if selected { "tonal" } else { "standard" },
                    id: "{option_id}",
                    role: "option",
                    aria_selected,
                    "data-testid": "{option_id}",
                    onclick: move |_| on_activate.call(()),
                    Flex { direction: "row", wrap: true, sx: value_sx,
                        Text { size: "xs", color: "text-dimmed", "{index}." }
                        Text { size: "sm", color: "text-dimmed", "{provider}" }
                        Text { size: "sm", "{model}" }
                        if let Some(label) = level_label {
                            Text { size: "xs", color: "text-dimmed", "{label}" }
                        }
                    }
                }
            }
        }
    }
}

/// A role's leader as the row's value: the model with its provider dimmed and
/// its thinking level, `auto` for an unset Default, or the role it inherits from.
fn leader_row_value(catalog: &ModelsCatalog, settings: &AgentSettingsView, role: &str) -> Element {
    match leader_value(settings, role) {
        LeaderValue::Assigned(selector) => {
            let (base, level) = split_entry(catalog, &selector);
            let (provider, model) = split_provider(base);
            let level_label = level.map(thinking_label);
            rsx! {
                Text { size: "sm", color: "muted", "{provider}" }
                Text { size: "sm", "{model}" }
                if let Some(label) = level_label {
                    Text { size: "xs", color: "muted", "{label}" }
                }
            }
        }
        LeaderValue::Auto => rsx! { Text { size: "sm", color: "muted", "auto" } },
        LeaderValue::Unset => {
            let inherits = role_inherits(role);
            rsx! { Text { size: "sm", color: "muted", "inherits {inherits}" } }
        }
    }
}

/// `provider/id` split at the first slash; a bare id has no provider.
fn split_provider(selector: &str) -> (&str, &str) {
    selector.split_once('/').unwrap_or(("", selector))
}
