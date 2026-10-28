//! The ordered fallback chain for one role: candidates in the order they are
//! tried, each with move and remove controls, plus an add dialog that searches
//! the models not yet in the chain. Entries are kept verbatim, so a stale
//! selector still shows and can be removed rather than being silently dropped.
use crate::components::agent_chat::model_picker::{ModelPicker, PickerOption};
use crate::components::layout::window_size::use_is_compact;
use crate::components::settings::model_chain::{
    move_entry_to_position, push_entry, split_thinking,
};
use crate::components::settings::model_chain_table::ModelChainTable;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Dialog, DialogBody, DialogContent, DialogHeader,
    DialogTitle, IconType,
};
use dioxus::prelude::*;
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
    /// The chain names search engines, not models.
    #[props(default)]
    engines: bool,
) -> Element {
    let compact = use_is_compact();
    let dialog_class = if compact {
        "top-auto bottom-0 left-0 right-0 w-full max-w-none max-h-[70vh] translate-x-0 translate-y-0"
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
    let edit_chain = chain.clone();
    let reorder_chain = chain.clone();
    let remove_chain = chain.clone();
    let reorder_change = on_change;
    let remove_change = on_change;
    rsx! {
        div { class: "flex flex-col gap-2", "data-testid": "{test_id}",
            if chain.is_empty() {
                p { class: "text-xs text-muted-foreground m-0", "Inherits {inherited_label}" }
            }
            if entry_count > 0 {
                ModelChainTable {
                    role_title: role_title.clone(),
                    chain: chain.clone(),
                    options: options
                            .clone(),
                    thinking_levels: thinking_levels.clone(),
                    test_id: format!("{test_id}-table"),
                    engines,
                    on_edit: move |index: usize| {
                        if edit_chain.get(index).is_some() {
                            editing.set(Some(index));
                            adding.set(true);
                        }
                    },
                    on_reorder: move |(source, target): (usize, usize)| {
                        let mut next = reorder_chain.clone();
                        if move_entry_to_position(&mut next, source, target) {
                            reorder_change.call(next);
                        }
                    },
                    on_remove: move |index: usize| {
                        let mut next = remove_chain.clone();
                        if index < next.len() {
                            next.remove(index);
                            remove_change.call(next);
                        }
                    },
                }
            }
            Button {
                variant: ButtonVariant::Outline,
                size: ButtonSize::Sm,
                icon: IconType::Plus,
                disabled: addable.is_empty(),
                title:
                if options.is_empty() {
                    "Connect a provider to choose a model"
                } else if addable.is_empty() {
                    "Every available model is already in this chain"
                } else {
                    "Search and choose the next model"
                },
                "data-testid": format!("{test_id}-add"),
                onclick: move |_| {
                    editing.set(None);
                    adding.set(true);
                },
                if entry_count > 0 {
                    "Add fallback"
                } else if engines {
                    "Choose engine"
                } else {
                    "Choose model"
                }
            }
            Dialog {
                open: adding(),
                on_open_change: move |open: bool| adding.set(open),
                DialogContent { class: dialog_class, test_id: format!("{test_id}-dialog"),
                    DialogHeader {
                        DialogTitle {
                            if editing().is_some() {
                                "Change {role_title}"
                            } else {
                                "Choose {role_title}"
                            }
                        }
                    }
                    DialogBody {
                        ModelPicker {
                            options: addable
                                    .clone(),
                            selected: selected_entry.clone(),
                            placeholder: "Search models and engines".to_owned(),
                            test_id: format!("{test_id}-picker"),
                            autofocus: !compact,
                            on_choose: move |selector: String| {
                                let mut next = chain.clone();
                                if let Some(index) = editing() {
                                    let Some(entry) = next.get_mut(index) else {
                                        adding.set(false);
                                        return;
                                    };
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
