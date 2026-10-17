//! The ordered fallback chain for one role: candidates in the order they are
//! tried, each with move and remove controls, plus an add dialog that searches
//! the models not yet in the chain. Entries are kept verbatim, so a stale
//! selector still shows and can be removed rather than being silently dropped.

use dioxus::prelude::*;

use crate::components::agent_chat::model_picker::{ModelPicker, PickerOption};
use crate::components::layout::window_size::use_is_compact;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Dialog, DialogBody, DialogContent, DialogHeader,
    DialogTitle, IconButton, IconType, List, ListRow, StatusDot,
};
use crate::components::settings::model_chain::{move_entry, push_entry, split_thinking};

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
    // A phone gets the picker as a bottom sheet, within a thumb's reach.
    let dialog_class = if compact {
        "top-auto bottom-0 left-0 right-0 w-full max-w-none max-h-[70vh] translate-x-0 translate-y-0 rounded-t-2xl rounded-b-none"
    } else {
        "max-w-md"
    };
    let mut adding = use_signal(|| false);
    let mut editing = use_signal(|| None::<usize>);
    let selected_entry = editing()
        .and_then(|index| chain.get(index))
        .map(|entry| split_thinking(entry, &thinking_levels).0.to_owned());
    let entry_count = chain.len();
    let already_chosen: Vec<String> = chain
        .iter()
        .map(|entry| split_thinking(entry, &thinking_levels).0.to_owned())
        .collect();
    let addable: Vec<PickerOption> = options
        .iter()
        .filter(|option| {
            !already_chosen.contains(&option.selector)
                || selected_entry.as_ref() == Some(&option.selector)
        })
        .cloned()
        .collect();
    rsx! {
        div { class: "flex flex-col gap-2", "data-testid": "{test_id}",
            if chain.is_empty() {
                p { class: "text-xs text-muted-foreground m-0", "Inherits {inherited_label}" }
            }
            if entry_count > 0 {
                List { contained: true,
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
                            rsx! {
                                ListRow {
                                    leading: (!known).then(|| rsx! {
                                        StatusDot {
                                            status: "warn".to_owned(),
                                            title: Some("Not available: this model is not in the catalog or its provider is not signed in".to_owned()),
                                        }
                                    }),
                                    headline: rsx! {
                                        Button {
                                            variant: ButtonVariant::Ghost,
                                            size: ButtonSize::Xs,
                                            disabled: options.is_empty(),
                                            title: format!("Change {role_title} candidate {}", index + 1),
                                            onclick: move |_| { editing.set(Some(index)); adding.set(true); },
                                            "{headline}"
                                        }
                                    },
                                    support: Some(rsx! {
                                        if index == 0 { "Primary" } else { "Fallback {index}" }
                                        if let Some(text) = support { " · {text}" }
                                    }),
                                    dense: true,
                                    test_id: row_test_id,
                                    trailing: Some(rsx! {
                                        IconButton {
                                            icon: IconType::ArrowUp,
                                            label: format!("Move {role_title} candidate {} up", index + 1),
                                            size: ButtonSize::IconSm,
                                            disabled: index == 0,
                                            onclick: move |_| {
                                                let mut next = up_chain.clone();
                                                if move_entry(&mut next, index, -1) {
                                                    up_change.call(next);
                                                }
                                            },
                                        }
                                        IconButton {
                                            icon: IconType::ArrowDown,
                                            label: format!("Move {role_title} candidate {} down", index + 1),
                                            size: ButtonSize::IconSm,
                                            disabled: index + 1 == entry_count,
                                            onclick: move |_| {
                                                let mut next = down_chain.clone();
                                                if move_entry(&mut next, index, 1) {
                                                    down_change.call(next);
                                                }
                                            },
                                        }
                                        IconButton {
                                            icon: IconType::X,
                                            label: format!("Remove {role_title} candidate {}", index + 1),
                                            size: ButtonSize::IconSm,
                                            onclick: move |_| {
                                                let mut next = remove_chain.clone();
                                                next.remove(index);
                                                remove_change.call(next);
                                            },
                                        }
                                    }),
                                }
                            }
                        }
                    }
                }
            }
            Button {
                variant: ButtonVariant::Outline,
                size: ButtonSize::Sm,
                icon: IconType::Plus,
                disabled: addable.is_empty(),
                title: if options.is_empty() { "Connect a provider to choose a model" } else if addable.is_empty() { "Every available model is already in this chain" } else { "Search and choose the next model" },
                "data-testid": format!("{test_id}-add"),
                onclick: move |_| { editing.set(None); adding.set(true); },
                if entry_count == 0 { "Choose model" } else { "Add fallback" }
            }
            Dialog {
                open: adding(),
                on_open_change: move |open: bool| adding.set(open),
                DialogContent {
                    class: dialog_class,
                    test_id: format!("{test_id}-dialog"),
                    DialogHeader {
                        DialogTitle {
                            if editing().is_some() { "Change {role_title}" } else { "Choose {role_title}" }
                        }
                    }
                    DialogBody {
                        ModelPicker {
                            options: addable.clone(),
                            selected: selected_entry.clone(),
                            placeholder: "Search models and engines".to_owned(),
                            test_id: format!("{test_id}-picker"),
                            autofocus: !compact,
                            on_choose: move |selector: String| {
                                let mut next = chain.clone();
                                if let Some(index) = editing() {
                                    let Some(entry) = next.get_mut(index) else { adding.set(false); return; };
                                    let effort = split_thinking(entry, &thinking_levels).1;
                                    *entry = match effort {
                                        Some(level) => format!("{selector}:{level}"),
                                        None => selector,
                                    };
                                    on_change.call(next);
                                } else if push_entry(&mut next, &selector) {
                                    on_change.call(next);
                                }
                                adding.set(false);
                            },
                        }
                    }
                }
            }
        }
    }
}
