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
use crate::components::agent_chat::agent_alert::ERROR_TEXT_CLASS;
use crate::components::agent_chat::model_controls::thinking_label;
use crate::components::agent_chat::model_picker::ModelPicker;
use crate::components::file_viewer::dom::focus_by_id;
use crate::components::settings::agent_roles::role_options;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Chip, IconType, TextField};
use crate::pump::Pump;

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

    rsx! {
        div {
            id: ROOT_ID,
            class: "flex min-h-0 flex-1 flex-col gap-2 outline-none",
            tabindex: "0",
            role: "region",
            "aria-label": "Model roles",
            onkeydown: move |event: KeyboardEvent| handle_key(&key_context, &event),
            p { class: "text-sm text-muted-foreground", "Model roles — f adds a fallback, x clears a role" }
            if !error_text.is_empty() {
                p { class: ERROR_TEXT_CLASS, "{error_text}" }
            }
            div { class: "flex flex-wrap gap-1", role: "tablist", "aria-label": "Role filter",
                for choice in RoleTab::ALL {
                    TabButton {
                        key: "{choice.label()}",
                        choice,
                        selected: tab() == choice,
                        on_select: move |_| {
                            let mut tab_signal = tab;
                            tab_signal.set(choice);
                            let mut cursor_signal = cursor;
                            cursor_signal.set(0);
                        },
                    }
                }
            }
            div { class: "flex flex-wrap items-center gap-2", role: "group", "aria-label": "Presets",
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
                        label: "Save as…".to_owned(),
                        icon: Some(IconType::Plus),
                        test_id: Some("agent-hub-roles-preset-save".to_owned()),
                        title: Some("Save these roles as a preset".to_owned()),
                        onclick: Some(EventHandler::new(move |_| {
                            open_context.run(RoleAction::OpenNaming);
                        })),
                    }
                }
            }
            if naming() {
                div { class: "flex flex-wrap items-end gap-2",
                    TextField {
                        value: preset_name(),
                        on_input: move |text: String| {
                            let mut field = preset_name;
                            field.set(text);
                        },
                        placeholder: "Preset name".to_owned(),
                        aria_label: "Preset name".to_owned(),
                        autocomplete: "off".to_owned(),
                        max_length: Some(64),
                        test_id: NAME_FIELD_ID.to_owned(),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter && !event.is_composing() {
                                event.prevent_default();
                                if !save_disabled {
                                    field_context.save_preset(&save_typed);
                                }
                            }
                        },
                    }
                    Button {
                        variant: ButtonVariant::Default,
                        size: ButtonSize::Sm,
                        disabled: save_disabled,
                        "data-testid": "agent-hub-roles-preset-save-confirm",
                        onclick: move |_| save_context.save_preset(&typed),
                        "Save"
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        onclick: move |_| cancel_context.cancel_naming(),
                        "Cancel"
                    }
                }
            }
            div { class: "flex min-h-0 flex-1 flex-col overflow-y-auto rounded-md border border-border", role: "listbox", "aria-label": "Roles", tabindex: "0", "aria-activedescendant": format!("agent-hub-roles-option-{cursor_row}"),
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
            if let Some(target) = picker_target {
                div { class: "shrink-0",
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
            p { class: "shrink-0 border-t border-border pt-2 text-xs text-muted-foreground", "{KEY_LEGEND}" }
        }
    }
}

/// One role-tab button. Clicks select the tab; the parent resets the cursor.
#[component]
fn TabButton(choice: RoleTab, selected: bool, on_select: EventHandler<()>) -> Element {
    rsx! {
        Button {
            variant: if selected { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
            size: ButtonSize::Sm,
            class: "rounded-full",
            role: "tab",
            "aria-selected": if selected { "true" } else { "false" },
            onclick: move |_| on_select.call(()),
            "{choice.label()}"
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
            label,
            icon: if is_active { Some(IconType::Check) } else { None },
            selected: Some(is_active),
            test_id: Some(test_id),
            title: Some(format!("Use {name} for every role, in all folders")),
            onclick: Some(EventHandler::new(move |_| on_apply.call(name.clone()))),
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
    let row_class = if selected {
        "flex flex-wrap items-center gap-2 px-2 py-1 bg-accent text-accent-foreground cursor-pointer"
    } else {
        "flex flex-wrap items-center gap-2 px-2 py-1 hover:bg-accent/50 cursor-pointer"
    };
    match row {
        RolesRow::Separator => rsx! {
            div { class: "my-1 h-px bg-border", role: "presentation", "aria-hidden": "true" }
        },
        RolesRow::NewFallback => rsx! {
            div {
                id: option_id,
                class: "{row_class} text-muted-foreground",
                role: "option",
                "aria-selected": aria_selected,
                onclick: move |_| on_activate.call(()),
                "+ New fallback…"
            }
        },
        RolesRow::Role { role } => {
            let (tag, tone) = role_tag(role);
            let value = leader_row_value(&catalog, &settings, role);
            rsx! {
                div {
                    id: option_id,
                    class: "{row_class}",
                    role: "option",
                    "aria-selected": aria_selected,
                    onclick: move |_| on_activate.call(()),
                    span { class: tone_class(tone), "{tag}" }
                    {value}
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
                div {
                    id: option_id,
                    class: "{row_class} pl-6",
                    role: "option",
                    "aria-selected": aria_selected,
                    onclick: move |_| on_activate.call(()),
                    span { class: "font-mono text-xs text-muted-foreground", "{index}." }
                    span { class: "text-muted-foreground", "{provider}" }
                    span { class: "font-medium", "{model}" }
                    if let Some(label) = level_label {
                        span { class: "text-xs text-muted-foreground", "{label}" }
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
                span { class: "text-muted-foreground", "{provider}" }
                span { class: "font-medium", "{model}" }
                if let Some(label) = level_label {
                    span { class: "text-xs text-muted-foreground", "{label}" }
                }
            }
        }
        LeaderValue::Auto => rsx! {
            span { class: "text-muted-foreground italic", "auto" }
        },
        LeaderValue::Unset => {
            let inherits = role_inherits(role);
            rsx! {
                span { class: "text-muted-foreground italic", "inherits {inherits}" }
            }
        }
    }
}

/// `provider/id` split at the first slash; a bare id has no provider.
fn split_provider(selector: &str) -> (&str, &str) {
    selector.split_once('/').unwrap_or(("", selector))
}

/// Tone colours come from the theme roles so the tags follow the active scheme.
fn tone_class(tone: RoleTone) -> &'static str {
    match tone {
        RoleTone::Ok => "rounded px-1.5 font-mono text-xs text-success",
        RoleTone::Warn => "rounded px-1.5 font-mono text-xs text-warning",
        RoleTone::Accent => "rounded px-1.5 font-mono text-xs text-primary",
        RoleTone::Muted => "rounded px-1.5 font-mono text-xs text-muted-foreground",
        RoleTone::Dim => "rounded px-1.5 font-mono text-xs text-muted-foreground/60",
    }
}
