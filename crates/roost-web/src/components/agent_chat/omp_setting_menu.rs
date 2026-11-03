//! OMP's composer picker combines model and effort in one compact trigger.
//! The anchored Command owns search and keyboard navigation; source and inherited
//! values stay inside the picker. Every choice comes from the worker's ACP catalog.

use super::model_controls::thinking_label;
use super::omp_controls::{choice_label, source_label};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Command, CommandEmpty, CommandInput, CommandItem,
    CommandList, Icon, IconType, Popover, PopoverContent, PopoverTrigger,
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    OmpChoice, OmpEffectiveValue, OmpSettingSource, split_omp_model,
};

#[component]
pub(super) fn OmpSettingMenu(
    trigger_id: String,
    model: OmpEffectiveValue,
    thinking: OmpEffectiveValue,
    inherited_model: OmpEffectiveValue,
    inherited_thinking: OmpEffectiveValue,
    models: Vec<OmpChoice>,
    thinking_levels: Vec<OmpChoice>,
    model_disabled: bool,
    thinking_disabled: bool,
    on_open: EventHandler<()>,
    on_model: EventHandler<Option<String>>,
    on_thinking: EventHandler<Option<String>>,
) -> Element {
    let mut open = use_signal(|| false);
    let mut effort_open = use_signal(|| false);
    let mut query = use_signal(String::new);
    let list_id = use_hook(|| crate::components::ui::field::scoped_element_id("omp-model-choices"));
    let model_name = models
        .iter()
        .find(|choice| Some(&choice.value) == model.value.as_ref())
        .map(|choice| choice.name.clone())
        .unwrap_or_else(|| {
            model.value.as_deref().map_or_else(
                || "Choose model".to_owned(),
                |value| {
                    split_omp_model(value).map_or_else(|| value.to_owned(), |model| model.model_id)
                },
            )
        });
    let effort = thinking.value.as_deref().map(thinking_label);
    let title = format!(
        "{} · {} · Thinking: {} · {}",
        model.value.as_deref().unwrap_or("OMP's own config"),
        source_label(&model.source),
        thinking.value.as_deref().unwrap_or("OMP's own config"),
        source_label(&thinking.source),
    );
    let choosing_effort = effort_open();
    let (choices, current, inherited, disabled, on_choose) = if choosing_effort {
        (
            thinking_levels,
            &thinking,
            &inherited_thinking,
            thinking_disabled,
            on_thinking,
        )
    } else {
        (models, &model, &inherited_model, model_disabled, on_model)
    };
    let inherited_selected = current.source != OmpSettingSource::Conversation;
    let inherited_description = format!(
        "{} · {}",
        inherited.value.as_deref().unwrap_or("OMP's own config"),
        source_label(&inherited.source),
    );
    let search = query().trim().to_lowercase();
    let filtered: Vec<_> = choices
        .into_iter()
        .filter(|choice| {
            choice.name.to_lowercase().contains(&search)
                || choice.value.to_lowercase().contains(&search)
        })
        .collect();
    let close_picker = EventHandler::new(move |()| {
        open.set(false);
        effort_open.set(false);
        query.set(String::new());
    });
    rsx! {
        Popover {
            trigger_id,
            open: open(),
            on_open_change: move |next| {
                open.set(next);
                if next { on_open.call(()); }
                else { close_picker.call(()); }
            },
            PopoverTrigger {
                ghost: true,
                disabled: model_disabled && thinking_disabled,
                aria_label: format!("OMP model and thinking level: {model_name} · {}", effort.as_deref().unwrap_or("Default")),
                title,
                class: "min-w-0 max-w-full",
                test_id: "omp-model-picker",
                span { class: "flex min-w-0 items-center gap-2",
                    span { class: "min-w-0 truncate", "{model_name}" }
                    if let Some(level) = effort {
                        span { class: "shrink-0 text-muted-foreground", "{level}" }
                    }
                }
                Icon { icon: IconType::ChevronDown, class: "size-4 shrink-0" }
            }
            PopoverContent { class: "w-72 max-w-full",
                div { class: "flex items-center justify-between gap-2",
                    if choosing_effort {
                        Button { variant: ButtonVariant::Ghost, size: ButtonSize::Sm,
                            aria_label: "Back to models",
                            onclick: move |_| { effort_open.set(false); query.set(String::new()); },
                            Icon { icon: IconType::ArrowLeft, class: "size-4" }
                            "Thinking level"
                        }
                    } else {
                        span { class: "text-sm font-medium", "Model" }
                        Button { variant: ButtonVariant::Ghost, size: ButtonSize::Sm,
                            disabled: thinking_disabled,
                            onclick: move |_| { effort_open.set(true); query.set(String::new()); },
                            "Thinking level"
                            Icon { icon: IconType::ChevronRight, class: "size-4" }
                        }
                    }
                }
                p { class: "text-xs text-muted-foreground", "From {source_label(&current.source)} · applies to your next message" }
                {rsx! { Command { key: "{choosing_effort}", should_filter: false, class: "h-auto",
                    CommandInput {
                        value: query(),
                        aria_label: if choosing_effort { "Search OMP thinking levels" } else { "Search OMP models" },
                        aria_controls: list_id.clone(),
                        placeholder: if choosing_effort { "Search thinking levels" } else { "Search models" },
                        autofocus: true,
                        on_search_change: move |next| query.set(next),
                        onkeydown: move |event: KeyboardEvent| {
                            if matches!(event.key(), Key::Named(NamedKey::Home | NamedKey::End)) {
                                event.stop_propagation();
                            }
                        },
                    }
                    CommandList { id: list_id.clone(), role: "listbox", class: "max-h-64 min-h-0 overflow-y-auto",
                        if !disabled && search.is_empty() {
                            CommandItem {
                                value: "inherit", show_check: false,
                                on_select: move |_| { on_choose.call(None); close_picker.call(()); },
                                span { class: "flex size-4 shrink-0 items-center justify-center",
                                    if inherited_selected { Icon { icon: IconType::Check, class: "size-4" } }
                                }
                                span { class: "flex min-w-0 flex-col",
                                    span { if choosing_effort { "Use inherited thinking" } else { "Use inherited model" } }
                                    span { class: "max-w-full truncate text-xs text-muted-foreground",
                                        "{inherited_description}"
                                    }
                                }
                            }
                        }
                        if !disabled {
                            for choice in filtered.iter() {
                                {
                                    let selected = !inherited_selected && current.value.as_deref() == Some(choice.value.as_str());
                                    let shown = if choosing_effort { thinking_label(&choice.name) } else { choice_label(choice) };
                                    let chosen = choice.value.clone();
                                    rsx! {
                                        CommandItem {
                                            key: "{choice.value}", value: choice.value.clone(), show_check: false,
                                            title: choice.description.clone().unwrap_or_else(|| choice.value.clone()),
                                            on_select: move |_| { on_choose.call(Some(chosen.clone())); close_picker.call(()); },
                                            span { class: "flex size-4 shrink-0 items-center justify-center",
                                                if selected { Icon { icon: IconType::Check, class: "size-4" } }
                                            }
                                            span { class: "min-w-0 truncate", "{shown}" }
                                        }
                                    }
                                }
                            }
                        }
                        if filtered.is_empty() || disabled {
                            CommandEmpty {
                                if search.is_empty() { "No options available from OMP" } else { "No options match this search" }
                            }
                        }
                    }
                }}}
            }
        }
    }
}
