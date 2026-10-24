//! The ordered fallback chain for one role: candidates in the order they are
//! tried, each with move and remove controls, plus an add dialog that searches
//! the models not yet in the chain. Entries are kept verbatim, so a stale
//! selector still shows and can be removed rather than being silently dropped.

use dioxus::prelude::*;
use libero::components::{ActionIcon, Button, Dialog, Flex, Indicator, Text};
use libero::hooks::{ModalScope, use_modal};
use std::{cell::RefCell, rc::Rc};

use crate::components::agent_chat::model_picker::{ModelPicker, PickerOption};
use crate::components::layout::window_size::use_is_compact;
use crate::components::overlay_generation::OverlayGeneration;
use crate::components::settings::model_chain::{move_entry, push_entry, split_thinking};

#[derive(Clone)]
struct ModelPickerRequest {
    chain: Vec<String>,
    options: Vec<PickerOption>,
    thinking_levels: Vec<String>,
    editing: Option<usize>,
    role_title: String,
}

fn apply_picker_choice(
    mut chain: Vec<String>,
    editing: Option<usize>,
    thinking_levels: &[String],
    selector: String,
) -> Option<Vec<String>> {
    if let Some(index) = editing {
        let entry = chain.get_mut(index)?;
        let effort = split_thinking(entry, thinking_levels).1;
        *entry = match effort {
            Some(level) => format!("{selector}:{level}"),
            None => selector,
        };
        Some(chain)
    } else if push_entry(&mut chain, &selector) {
        Some(chain)
    } else {
        None
    }
}

fn picker_options(
    chain: &[String],
    options: &[PickerOption],
    thinking_levels: &[String],
    editing: Option<usize>,
) -> Vec<PickerOption> {
    let already_chosen: Vec<String> = chain
        .iter()
        .map(|entry| split_thinking(entry, thinking_levels).0.to_owned())
        .collect();
    let selected_entry = editing
        .and_then(|index| chain.get(index))
        .map(|entry| split_thinking(entry, thinking_levels).0);
    options
        .iter()
        .filter(|option| {
            !already_chosen.contains(&option.selector)
                || selected_entry == Some(option.selector.as_str())
        })
        .cloned()
        .collect()
}

/// Edits one role's chain. `on_change` receives the whole new chain each time,
/// so the caller decides how it is saved and scoped.
#[component]
pub fn ModelChainEditor(
    role_title: String,
    chain: Vec<String>,
    options: Vec<PickerOption>,
    thinking_levels: Vec<String>,
    inherited_label: String,
    on_change: EventHandler<Vec<String>>,
    test_id: String,
) -> Element {
    let compact = use_is_compact();
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let modal_test_id = test_id.clone();
    let modal = use_modal(move |scope: ModalScope<ModelPickerRequest, String>| {
        let Some(request) = scope.try_args() else {
            return rsx! {};
        };
        let addable = picker_options(
            &request.chain,
            &request.options,
            &request.thinking_levels,
            request.editing,
        );
        let selected = request
            .editing
            .and_then(|index| request.chain.get(index))
            .map(|entry| split_thinking(entry, &request.thinking_levels).0.to_owned());
        rsx! {
            Dialog { title: if request.editing.is_some() { format!("Change {}", request.role_title) } else { format!("Choose {}", request.role_title) }, size: "md",
                "data-testid": "{modal_test_id}-dialog",
                ModelPicker {
                    options: addable,
                    selected,
                    placeholder: "Search models and engines".to_owned(),
                    test_id: format!("{modal_test_id}-picker"),
                    autofocus: !compact,
                    on_choose: move |selector: String| scope.resolve(selector),
                }
            }
        }
    });
    let addable = picker_options(&chain, &options, &thinking_levels, None);
    let entry_count = chain.len();
    let modal_for_drop = modal;
    let open_picker = {
        let generations = Rc::clone(&generation);
        let chain = chain.clone();
        let options = options.clone();
        let thinking_levels = thinking_levels.clone();
        let picker_role_title = role_title.clone();
        move |editing| {
            let token = generations.borrow_mut().begin();
            let completion_generation = Rc::clone(&generations);
            let completion_chain = chain.clone();
            let completion_levels = thinking_levels.clone();
            modal
                .open_with(ModelPickerRequest {
                    chain: chain.clone(),
                    options: options.clone(),
                    thinking_levels: thinking_levels.clone(),
                    editing,
                    role_title: picker_role_title.clone(),
                })
                .onresult(move |result| {
                    if !completion_generation.borrow_mut().finish(token) {
                        return;
                    }
                    if let Some(selector) = result
                        && let Some(next) = apply_picker_choice(
                            completion_chain.clone(),
                            editing,
                            &completion_levels,
                            selector,
                        )
                    {
                        on_change.call(next);
                    }
                });
        }
    };
    let open_picker = Rc::new(open_picker);
    use_drop({
        let generation = Rc::clone(&generation);
        move || {
            generation.borrow_mut().invalidate();
            modal_for_drop.close();
        }
    });
    let add_open_picker = Rc::clone(&open_picker);
    rsx! {
        Flex { direction: "column", gap: "sm", "data-testid": "{test_id}",
            if chain.is_empty() {
                Text { size: "xs", color: "muted", "Inherits {inherited_label}" }
            }
            if entry_count > 0 {
                Flex { direction: "column", gap: "xs",
                    for (index, entry) in chain.iter().enumerate() {
                        {
                            let (base, thinking) = split_thinking(entry, &thinking_levels);
                            let option = options.iter().find(|option| option.selector == base);
                            let known = option.is_some() || base.starts_with('@');
                            let headline = option.map_or_else(|| base.to_owned(), |option| option.name.clone());
                            let support = match (option.and_then(|option| option.detail.clone()), thinking) {
                                (Some(detail), Some(level)) => Some(format!("{detail} · thinking {level}")),
                                (Some(detail), None) => Some(detail),
                                (None, Some(level)) => Some(format!("{base} · thinking {level}")),
                                (None, None) => Some(base.to_owned()),
                            };
                            let up_chain = chain.clone();
                            let down_chain = chain.clone();
                            let remove_chain = chain.clone();
                            let up_change = on_change;
                            let down_change = on_change;
                            let remove_change = on_change;
                            let row_test_id = format!("{test_id}-entry-{index}");
                            let row_open_picker = Rc::clone(&open_picker);
                            rsx! {
                                Flex { direction: "row", align: "center", gap: "sm",
                                    "data-testid": "{row_test_id}",
                                    if !known {
                                        Indicator {
                                            color: "warning",
                                            size: "xs",
                                            title: "Not available: this model is not in the catalog or its provider is not signed in",
                                            "aria-label": "Model not available",
                                        }
                                    }
                                    Flex { direction: "column", gap: "xs",
                                        Button {
                                            variant: "standard",
                                            color: "muted",
                                            size: "xs",
                                            disabled: options.is_empty(),
                                            title: format!("Change {role_title} candidate {}", index + 1),
                                            onclick: move |_| row_open_picker(Some(index)),
                                            "{headline}"
                                        }
                                        Text { size: "xs", color: "muted",
                                            if index == 0 { "Primary" } else { "Fallback {index}" }
                                            if let Some(text) = support { " · {text}" }
                                        }
                                    }
                                    Flex { direction: "row", align: "center", gap: "xs",
                                        ActionIcon {
                                            icon: pictogram_icons_lucide::arrow_up::outlined,
                                            aria_label: format!("Move {role_title} candidate {} up", index + 1),
                                            size: "sm",
                                            disabled: index == 0,
                                            onclick: move |_| {
                                                let mut next = up_chain.clone();
                                                if move_entry(&mut next, index, -1) {
                                                    up_change.call(next);
                                                }
                                            },
                                        }
                                        ActionIcon {
                                            icon: pictogram_icons_lucide::arrow_down::outlined,
                                            aria_label: format!("Move {role_title} candidate {} down", index + 1),
                                            size: "sm",
                                            disabled: index + 1 == entry_count,
                                            onclick: move |_| {
                                                let mut next = down_chain.clone();
                                                if move_entry(&mut next, index, 1) {
                                                    down_change.call(next);
                                                }
                                            },
                                        }
                                        ActionIcon {
                                            icon: pictogram_icons_lucide::x::outlined,
                                            aria_label: format!("Remove {role_title} candidate {}", index + 1),
                                            size: "sm",
                                            onclick: move |_| {
                                                let mut next = remove_chain.clone();
                                                next.remove(index);
                                                remove_change.call(next);
                                            },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Button {
                variant: "outlined",
                size: "sm",
                icon: Some(rsx! { libero::components::Pictogram { icon: pictogram_icons_lucide::plus::outlined } }),
                disabled: addable.is_empty(),
                title: if options.is_empty() { "Connect a provider to choose a model" } else if addable.is_empty() { "Every available model is already in this chain" } else { "Search and choose the next model" },
                "data-testid": format!("{test_id}-add"),
                onclick: move |_| add_open_picker(None),
                if entry_count == 0 { "Choose model" } else { "Add fallback" }
            }
        }
    }
}
