//! The composer's conversation model and thinking selector. It reads real
//! catalog capabilities, leaves global roles to the inspector, and reports errors.
use super::model_controls::{current_model_label, thinking_label};
use super::model_reference_actions::{
    available_and_locked_models, configure_model, filter_model_rows,
};
use crate::components::ui::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::ui::dropdown_menu_sub::{
    DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubItem, DropdownMenuSubSide,
    DropdownMenuSubTrigger,
};
use crate::components::ui::menu_overlay::MenuSide;
use crate::components::ui::{Icon, IconType, Input, InputType};
use crate::pump::Pump;
use crate::router_state::use_navigate;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

#[component]
pub(super) fn ModelReferenceMenu(
    trigger_id: String,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    catalog_failed: bool,
    pump: Pump,
) -> Element {
    let mut query = use_signal(String::new);
    let mut open = use_signal(|| false);
    let mut submenu_open = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut pending = use_signal(|| false);
    let current_label = current_model_label(catalog.as_ref(), &conversation);
    let trigger_label = current_label
        .strip_suffix(" · Default")
        .unwrap_or(&current_label)
        .to_owned();
    let effective = conversation
        .effective_model
        .as_ref()
        .or(conversation.model.as_ref());
    let selected = catalog.as_ref().and_then(|catalog| {
        effective.and_then(|current| {
            catalog.models.iter().find(|model| {
                model.provider == current.provider && model.model_id == current.model_id
            })
        })
    });
    let selected_levels = selected
        .filter(|model| model.reasoning)
        .map(|model| model.thinking_levels.as_slice())
        .unwrap_or_default();
    let thinking = conversation
        .thinking_level
        .clone()
        .filter(|level| selected_levels.iter().any(|supported| supported == level));
    let model_rows = catalog
        .as_ref()
        .map(available_and_locked_models)
        .unwrap_or_default();
    let no_connected_models = model_rows.iter().all(|model| !model.available);
    let navigate = use_navigate();
    let settings_href = crate::routes::settings_pane_href("models");
    let query_value = query();
    let filtered_models = filter_model_rows(catalog.as_ref(), &model_rows, &query_value);
    let no_search_results = !query_value.is_empty() && filtered_models.is_empty();
    let submenu_levels = selected_levels.to_vec();
    let can_choose_thinking = !submenu_levels.is_empty();
    let conversation_id = conversation.id.clone();
    let configure_pump = pump.clone();
    let title_label = trigger_label.clone();
    let model_trigger_id = trigger_id.clone();
    let selected_selector = effective.map(|model| format!("{}/{}", model.provider, model.model_id));
    let inherited = conversation.model.is_none();
    let on_open_change = move |next| {
        open.set(next);
        if !next {
            submenu_open.set(false);
        }
    };
    let choose_model = move |selector: Option<String>| {
        pending.set(true);
        error.set(String::new());
        configure_model(
            configure_pump.clone(),
            conversation_id.clone(),
            selector,
            None,
            pending,
            error,
        );
    };
    let thinking_pump = pump.clone();
    let thinking_conversation = conversation.id.clone();
    let choose_thinking = move |level: String| {
        pending.set(true);
        error.set(String::new());
        configure_model(
            thinking_pump.clone(),
            thinking_conversation.clone(),
            None,
            Some(level),
            pending,
            error,
        );
    };
    let thinking_title = thinking.as_deref().map(thinking_label);
    let trigger_aria_label = match thinking_title.as_deref() {
        Some(level) => format!("Model and thinking level: {title_label} · {level}"),
        None => format!("Model and thinking level: {title_label}"),
    };
    rsx! {
        div { class: "relative flex min-w-0 max-w-full flex-col",
            DropdownMenu {
                open: open(),
                on_open_change,
                side: MenuSide::Above,
                trigger_id: Some(model_trigger_id),
                menu_id: Some(format!("{trigger_id}-model-menu")),
                DropdownMenuTrigger {
                    aria_label: trigger_aria_label,
                    class: Some("min-w-0 max-w-full".to_owned()),
                    test_id: "agent-chat-model-picker",
                    title: format!("{title_label} · Model and thinking level"),
                    span { class: "inline-flex min-w-0 items-center gap-2",
                        span { class: "max-w-28 truncate", "{trigger_label}" }
                        if let Some(level) = thinking_title.as_deref() {
                            span { class: "shrink-0", "{level}" }
                        }
                    }
                    Icon { icon: IconType::ChevronDown }
                }
                DropdownMenuContent {
                    class: Some("w-64".to_owned()),
                    if catalog.is_some() {
                        Input {
                            r#type: InputType::Search,
                            aria_label: "Search models",
                            placeholder: "Search models",
                            value: Some(query_value),
                            class: Some("mb-1".to_owned()),
                            oninput: Some(EventHandler::new(move |event: FormEvent| query.set(event.value()))),
                        }
                        div { class: "max-h-72 overflow-y-auto",
                            DropdownMenuItem {
                                disabled: pending(),
                                onclick: Some(EventHandler::new({
                                    let mut choose_model = choose_model.clone();
                                    move |_| choose_model(None)
                                })),
                                span { class: "flex min-w-0 flex-1 items-center gap-2",
                                    if inherited { Icon { icon: IconType::Check, class: Some("size-4 shrink-0".to_owned()) } }
                                    else { span { class: "size-4 shrink-0" } }
                                    span { class: "truncate", "Use chat role" }
                                }
                            }
                            for model in filtered_models {
                                {
                                    let selector = format!("{}/{}", model.provider, model.model_id);
                                    let selected_now = selected_selector.as_deref() == Some(selector.as_str()) && !inherited;
                                    let available = model.available;
                                    let provider = catalog.as_ref().and_then(|catalog| {
                                        catalog.providers.iter().find(|entry| entry.id == model.provider)
                                    }).map_or(model.provider.as_str(), |entry| entry.name.as_str());
                                    let mut choose_model = choose_model.clone();
                                    rsx! {
                                        DropdownMenuItem {
                                            key: "{selector}",
                                            disabled: !available || pending(),
                                            title: Some(format!("{} · {}", model.name, provider)),
                                            onclick: Some(EventHandler::new(move |_| {
                                                if available { choose_model(Some(selector.clone())); }
                                            })),
                                            span { class: "flex min-w-0 flex-1 items-center gap-2",
                                                if !available { Icon { icon: IconType::Lock, class: Some("size-4 shrink-0".to_owned()) } }
                                                else { span { class: "size-4 shrink-0" } }
                                                span { class: "truncate", "{model.name}" }
                                                if selected_now { Icon { icon: IconType::Check, class: Some("size-4 shrink-0".to_owned()) } }
                                            }
                                            span { class: "max-w-20 truncate text-xs text-muted-foreground", "{provider}" }
                                        }
                                    }
                                }
                            }
                            if no_search_results {
                                p { class: "px-2 py-2 text-sm text-muted-foreground", "No models match this search" }
                            }
                        }
                    }
                    if catalog.is_none() {
                        p { class: "px-2 py-2 text-sm text-muted-foreground", role: "status",
                            if catalog_failed { "Models unavailable" } else { "Loading models…" }
                        }
                    } else if model_rows.is_empty() {
                        p { class: "px-2 py-2 text-sm text-muted-foreground", role: "status",
                            "No chat models are listed"
                        }
                    }
                    if no_connected_models {
                        DropdownMenuItem {
                            onclick: Some(EventHandler::new(move |_| navigate.call(settings_href.clone()))),
                            Icon { icon: IconType::Link }
                            "Open model settings"
                        }
                    }
                    if can_choose_thinking {
                        DropdownMenuSub {
                            open: submenu_open(),
                            on_open_change: move |next| submenu_open.set(next),
                            side: DropdownMenuSubSide::Right,
                            DropdownMenuSubTrigger {
                                disabled: pending(),
                                class: Some("w-full".to_owned()),
                                "Thinking level"
                            }
                            DropdownMenuSubContent {
                                class: Some("max-h-64 w-44 overflow-y-auto".to_owned()),
                                for (index, level) in submenu_levels.iter().enumerate() {
                                    {
                                        let mut choose_thinking = choose_thinking.clone();
                                        let chosen_level = level.clone();
                                        let selected_now = thinking.as_deref() == Some(level.as_str());
                                        rsx! {
                                            DropdownMenuSubItem {
                                                disabled: pending(),
                                                test_id: Some(format!("agent-chat-thinking-level-{index}")),
                                                on_select: Some(EventHandler::new(move |_| {
                                                    choose_thinking(chosen_level.clone())
                                                })),
                                                if selected_now {
                                                    Icon { icon: IconType::Check }
                                                } else {
                                                    span { class: "size-4 shrink-0" }
                                                }
                                                span { class: "truncate", "{thinking_label(level)}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !error().is_empty() {
                p { class: "max-w-64 text-xs text-destructive", role: "alert", "{error()}" }
            }
        }
    }
}
