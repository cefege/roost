//! A searchable list of models grouped by provider, shared by the session
//! picker, the models hub and the role chain's add dialog. By default it lists
//! only the newest version of each model ("Claude Sonnet 5.5", not every
//! Sonnet before it); a "Latest only" chip shows them all. Pure filtering and
//! grouping sit beside the component so the rules have one reader. The picker
//! only reports the chosen selector; what that choice means is the caller's.

use std::collections::HashSet;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{model_family, model_search_text, rank_matches};

use crate::components::file_viewer::dom::focus_by_id;
use libero::components::{
    Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Text, TextField, use_combobox,
};
use libero::sx::sx;

/// One choice the picker offers. `selector` is the saved string (`provider/id`
/// for a model, `web/<engine>` for a search engine).
#[derive(Debug, Clone, PartialEq)]
pub struct PickerOption {
    pub selector: String,
    pub name: String,
    pub group: String,
    pub detail: Option<String>,
}

/// Options matching every whitespace-separated term of `query`, best match
/// first (`rank_matches`: whole word, prefix, substring, then scattered
/// letters), ties in their given order. An empty query keeps every option in
/// order.
pub fn filter_options<'a>(options: &'a [PickerOption], query: &str) -> Vec<&'a PickerOption> {
    let haystacks: Vec<String> = options
        .iter()
        .map(|option| {
            let name = format!(
                "{} {} {}",
                option.name,
                option.group,
                option.detail.as_deref().unwrap_or_default()
            );
            model_search_text(&option.selector, &name)
        })
        .collect();
    rank_matches(&haystacks, query)
        .into_iter()
        .map(|index| &options[index])
        .collect()
}

/// Groups options by their group name, keeping first-seen group order.
pub fn group_options<'a>(options: Vec<&'a PickerOption>) -> Vec<(String, Vec<&'a PickerOption>)> {
    let mut groups: Vec<(String, Vec<&'a PickerOption>)> = Vec::new();
    for option in options {
        match groups.iter_mut().find(|(group, _)| *group == option.group) {
            Some((_, rows)) => rows.push(option),
            None => groups.push((option.group.clone(), vec![option])),
        }
    }
    groups
}

/// For each `(selector, name)` in a newest-first list, whether a newer version
/// of the same model from the same provider comes before it (see
/// `model_family`). Selectors in `keep` (the model in use, role leaders) are
/// never superseded; a kept model that is the newest of its family still
/// supersedes the older ones after it.
pub fn superseded<'a>(
    items: impl IntoIterator<Item = (&'a str, &'a str)>,
    keep: &[String],
) -> Vec<bool> {
    let mut newest: HashSet<(String, String)> = HashSet::new();
    items
        .into_iter()
        .map(|(selector, name)| {
            let (provider, id) = selector.split_once('/').unwrap_or(("", selector));
            let first = newest.insert((provider.to_owned(), model_family(name, id)));
            !first && !keep.iter().any(|kept| kept == selector)
        })
        .collect()
}

/// The search field, then the matching options under their provider headings.
/// `selected` marks the current choice; choosing a row calls `on_choose` with
/// its selector. Older versions are hidden until the reader turns off "Latest
/// only"; `selected` and `keep` always show. `autofocus` is off on a phone,
/// where focusing the field would raise the soft keyboard over the list
/// before the reader asked to type.
#[component]
pub fn ModelPicker(
    options: Vec<PickerOption>,
    selected: Option<String>,
    on_choose: EventHandler<String>,
    placeholder: String,
    test_id: String,
    #[props(default = true)] autofocus: bool,
    #[props(default)] keep: Vec<String>,
) -> Element {
    let mut query = use_signal(String::new);
    let mut show_all = use_signal(|| false);
    let combobox = use_combobox();
    let current_query = query();
    let mut kept = keep;
    kept.extend(selected.clone());
    let older = superseded(
        options
            .iter()
            .map(|option| (option.selector.as_str(), option.name.as_str())),
        &kept,
    );
    let hidden = older.iter().filter(|older| **older).count();
    let listed: Vec<PickerOption> = options
        .iter()
        .zip(&older)
        .filter(|(_, is_older)| show_all() || !**is_older)
        .map(|(option, _)| option.clone())
        .collect();
    let groups = group_options(filter_options(&listed, &current_query));
    let choices: Vec<PickerOption> = groups
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|option| (**option).clone()))
        .collect();
    let selected_index = choices
        .iter()
        .position(|choice| selected.as_ref() == Some(&choice.selector));
    let no_matches = choices.is_empty();
    let older_match = no_matches
        && !show_all()
        && hidden > 0
        && !filter_options(&options, &current_query).is_empty();
    let versions_note = if show_all() {
        format!("Showing {hidden} older versions")
    } else {
        format!("{hidden} older versions hidden")
    };
    let picker_sx = use_hook(|| {
        sx().min_height("0")
            .display("flex")
            .flex_direction("column")
            .gap("sm")
    });
    let versions_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let notice_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("xs")
    });
    rsx! {
        Flex { sx: picker_sx, direction: "column", gap: "sm", "data-testid": "{test_id}",
            Combobox {
                state: combobox,
                options: choices,
                selected_index,
                option: move |row: ComboboxOptionArgs<PickerOption>| {
                    let selected = selected.as_ref() == Some(&row.value.selector);
                    let choice = row.value.selector.clone();
                    let name = row.value.name.clone();
                    let detail = row.value.detail.clone();
                    let state = combobox;
                    rsx! {
                        ComboboxOption {
                            selected,
                            onpick: move |_| {
                                on_choose.call(choice.clone());
                                state.close();
                            },
                            Text { size: "xs", color: "muted", "{row.value.group}" }
                            span { "data-slot": "label", "{name}" }
                            if let Some(detail) = detail {
                                Text { size: "sm", color: "muted", "{detail}" }
                            }
                            if selected { span { "aria-hidden": "true", "✓" } }
                        }
                    }
                },
                empty: rsx! { Text { color: "muted", "No matching models." } },
                TextField {
                    label: "Search models",
                    value: current_query.clone(),
                    oninput: move |text: String| {
                        query.set(text);
                        combobox.open();
                        combobox.set_active(Some(0));
                    },
                    onfocus: move |_| {
                        combobox.set_active(Some(selected_index.unwrap_or(0)));
                        combobox.open();
                    },
                    onblur: move |_| combobox.close(),
                    placeholder,
                    r#type: "search",
                    autocomplete: "off",
                    attributes: {
                        let mut attributes = combobox.a11y_attributes();
                        if autofocus {
                            attributes.push(Attribute::new("autofocus", true, None, false));
                        }
                        attributes
                    },
                    "data-testid": "{test_id}-search",
                }
            }
            if hidden > 0 {
                Flex { sx: versions_sx, direction: "row", align: "center", gap: "sm",
                    Button {
                        variant: "outlined",
                        selected: Some(!show_all()),
                        title: "Show only the newest version of each model",
                        "data-testid": "{test_id}-latest-only",
                        onclick: move |_| {
                            show_all.set(!show_all());
                            combobox.set_active(Some(0));
                        },
                        "Latest only"
                    }
                    Text { size: "sm", color: "muted", "{versions_note}" }
                }
            }
            if older_match {
                Flex { sx: notice_sx, direction: "row", align: "center", gap: "xs",
                    Text { color: "muted", "Only older versions match “{current_query}”." }
                    Button {
                        variant: "outlined",
                        "data-testid": "{test_id}-show-all",
                        onclick: {
                            let search_id = format!("{test_id}-search");
                            move |_| {
                                show_all.set(true);
                                focus_by_id(&search_id);
                            }
                        },
                        "Show all versions"
                    }
                }
            } else if no_matches {
                Text { color: "muted", "No models match “{current_query}”." }
            }
        }
    }
}
