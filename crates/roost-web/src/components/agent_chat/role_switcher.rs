//! The model panel the composer's model button opens: every model role this
//! chat runs (Default, Fast, Plan mode, helpers…) with the model leading it,
//! what each role is for, and an editor to change a role's model and thinking
//! level in place. An edit saves to all folders, or to this chat's folder
//! only; a role this folder already overrides opens on the folder. Mounted by
//! `model_controls`; reads and writes through `settings::agent_roles`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ConversationSummary, ModelsCatalog};

use super::agent_alert::ERROR_TEXT_CLASS;
use super::model_controls::configure_model_pin;
use super::model_picker::ModelPicker;
use super::role_presets::RolePresets;
use super::role_switch::{
    RoleLine, initial_scope, pin_hides, role_lines, thinking_choices, with_role_leader,
    with_role_thinking,
};
use super::role_thinking::RoleThinking;
use crate::components::layout::window_size::use_is_compact;
use crate::components::settings::agent_roles::{load_settings, role_options};
use crate::components::settings::model_scope::{ModelScope, with_role_value};
use crate::components::ui::anchored_panel::focus_element_by_id;
use crate::components::ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Chip, FieldError, IconButton, IconType,
    List, ListRow, Popover, PopoverContent, PopoverTrigger, Skeleton,
};
use crate::pump::Pump;
use crate::router_state::use_navigate;

/// The model button and the panel it opens: every model role this chat runs,
/// with an editor for each. Owns the open and editing state. Settings load on
/// each open, so a save elsewhere never shows stale here.
#[component]
pub(super) fn RoleSwitcher(
    trigger_id: String,
    trigger_label: String,
    trigger_title: String,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
) -> Element {
    let mut open = use_signal(|| false);
    let mut editing = use_signal(|| None::<RoleLine>);
    let settings = use_signal(AgentSettingsView::default);
    let settings_loaded = use_signal(|| false);
    let error = use_signal(String::new);
    let load_pump = pump.clone();
    use_effect(move || {
        if open() {
            load_settings(load_pump.clone(), settings, error, settings_loaded);
        }
    });
    let close = EventHandler::new(move |()| {
        open.set(false);
        editing.set(None);
    });
    let escape_trigger_id = trigger_id.clone();
    rsx! {
        Popover {
            open: open(),
            on_open_change: move |next: bool| {
                if !next {
                    editing.set(None);
                }
                open.set(next);
            },
            trigger_id,
            test_id: "agent-roles",
            // Escape steps back out of an editor before it closes the panel.
            on_escape: EventHandler::new(move |()| {
                focus_element_by_id(&escape_trigger_id);
                if editing.peek().is_some() {
                    editing.set(None);
                } else {
                    close.call(());
                }
            }),
            PopoverTrigger {
                class: "min-w-0 max-w-full",
                test_id: "agent-chat-model-trigger",
                title: trigger_title,
                span { class: "truncate", "{trigger_label}" }
            }
            PopoverContent {
                class: "w-80",
                if settings_loaded() {
                    RolePanel { editing, on_close: close, conversation, catalog, pump, settings, error }
                } else if !error().is_empty() {
                    FieldError { {error()} }
                } else {
                    Skeleton { class: "h-24 w-full" }
                }
            }
        }
    }
}

#[component]
fn RolePanel(
    editing: Signal<Option<RoleLine>>,
    on_close: EventHandler<()>,
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
                div { class: "flex items-center gap-2",
                    IconButton {
                        icon: IconType::ArrowLeft,
                        label: "All roles",
                        title: "All roles",
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::IconSm,
                        "data-testid": "agent-roles-back",
                        onclick: move |_| editing.set(None),
                    }
                    span { class: "text-sm font-semibold", "{line.title}" }
                }
                p { class: "text-sm text-muted-foreground", "{line.purpose}" }
                // Filter chips: the chosen one carries the check.
                div { class: "flex flex-wrap items-center gap-2", role: "group", "aria-label": "Applies to",
                    Chip { label: "All folders".to_owned(), selected: Some(!on_folder),
                        icon: (!on_folder).then_some(IconType::Check),
                        test_id: Some("agent-roles-scope-global".to_owned()),
                        onclick: Some(EventHandler::new(move |_| scope.set(ModelScope::Global))),
                    }
                    Chip { label: "This folder".to_owned(), selected: Some(on_folder),
                        icon: on_folder.then_some(IconType::Check),
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
                                reset_pump.save_agent_settings(updated, settings, error);
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
                        choose_pump.save_agent_settings(updated, settings, error);
                        // A session pin runs ahead of this role; clear it so this chat follows.
                        if clear_pin {
                            configure_model_pin(choose_pump.clone(), conversation_id.clone(), None);
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
                div { class: "flex items-center gap-2",
                    span { class: "text-sm font-semibold", "Models" }
                    IconButton {
                        icon: IconType::Info,
                        label: if show_purpose() { "Hide what each role is for" } else { "Show what each role is for" },
                        title: "What each role is for",
                        variant: if show_purpose() { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                        size: ButtonSize::IconSm,
                        "data-testid": "agent-roles-help",
                        onclick: move |_| show_purpose.toggle(),
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
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
                List {
                    for line in lines {
                        ListRow {
                            key: "{line.role}",
                            test_id: Some(format!("agent-roles-{}", line.role)),
                            class: Some(if show_purpose() { "py-2 items-start" } else { "py-2" }.to_owned()),
                            dense: true,
                            headline: rsx! { span { "{line.title}" } },
                            support: Some(rsx! {
                                span { "{line.model}" }
                                if show_purpose() {
                                    span { class: "text-muted-foreground", "{line.purpose}" }
                                }
                            }),
                            trailing: (line.this_chat || line.folder_override).then(|| rsx! {
                                if line.this_chat {
                                    span { title: "This chat's own turns run on this role",
                                        Badge { variant: BadgeVariant::Secondary, "This chat" }
                                    }
                                }
                                if line.folder_override {
                                    Badge { variant: BadgeVariant::Outline, "This folder" }
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
        div { class: "flex flex-col gap-3",
            {body}
            if !error().is_empty() {
                p { class: ERROR_TEXT_CLASS, role: "alert", "{error}" }
            }
        }
    }
}
