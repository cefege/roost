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
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Chip, Icon, List, ListRow, SectionTitle, TextField,
};

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
        .filter(|(_, older)| show_all() || !**older)
        .map(|(option, _)| option.clone())
        .collect();
    let groups = group_options(filter_options(&listed, &current_query));
    let keyboard_choices: Vec<String> = groups
        .iter()
        .flat_map(|(_, rows)| rows)
        .map(|option| option.selector.clone())
        .collect();
    let initial = keyboard_choices
        .iter()
        .position(|choice| selected.as_ref() == Some(choice))
        .unwrap_or(0);
    let mut active = use_signal(move || initial);
    let no_matches = keyboard_choices.is_empty();
    // Nothing new matches, but an older version would: say so, one tap away.
    let older_match = no_matches
        && !show_all()
        && hidden > 0
        && !filter_options(&options, &current_query).is_empty();
    let highlighted = active().min(keyboard_choices.len().saturating_sub(1));
    let results_id = format!("{test_id}-results");
    let active_id = (!no_matches).then(|| format!("{test_id}-option-{highlighted}"));
    let mut row_index = 0;
    let keyboard_id = test_id.clone();
    let search_id = format!("{test_id}-search-field");
    let search_focus = search_id.clone();
    let versions_note = if show_all() {
        format!("Showing {hidden} older versions")
    } else {
        format!("{hidden} older versions hidden")
    };
    rsx! {
        div { class: "model-picker", "data-testid": "{test_id}",
            TextField {
                value: current_query.clone(),
                on_input: move |text: String| {
                    query.set(text);
                    active.set(0);
                },
                aria_label: Some("Search models".to_owned()),
                placeholder: Some(placeholder),
                input_type: Some("search".to_owned()),
                autocomplete: Some("off".to_owned()),
                autofocus,
                class: Some("model-picker__search".to_owned()),
                test_id: Some(format!("{test_id}-search")),
                id: Some(search_id.clone()),
                role: "combobox",
                aria_controls: results_id.clone(),
                aria_expanded: "true",
                aria_active_descendant: active_id,
                onkeydown: move |event: KeyboardEvent| {
                    let count = keyboard_choices.len();
                    if count == 0 { return; }
                    let next = match event.key().to_string().as_str() {
                        "ArrowDown" => (highlighted + 1) % count,
                        "ArrowUp" => (highlighted + count - 1) % count,
                        "Home" => 0,
                        "End" => count - 1,
                        "Enter" => {
                            event.prevent_default();
                            on_choose.call(keyboard_choices[highlighted].clone());
                            return;
                        },
                        _ => return,
                    };
                    event.prevent_default();
                    active.set(next);
                    scroll_choice_into_view(&format!("{keyboard_id}-option-{next}"));
                },
            }
            if hidden > 0 {
                div { class: "model-picker__versions",
                    Chip {
                        label: "Latest only".to_owned(),
                        icon: Some("filter_list".to_owned()),
                        selected: Some(!show_all()),
                        small: true,
                        title: Some("Show only the newest version of each model".to_owned()),
                        test_id: Some(format!("{test_id}-latest-only")),
                        onclick: Some(EventHandler::new(move |()| {
                            let next = !show_all();
                            show_all.set(next);
                            active.set(0);
                        })),
                    }
                    span { class: "model-picker__versions-note", "{versions_note}" }
                }
            }
            if older_match {
                div { class: "model-picker__empty",
                    p { "Only older versions match “{current_query}”." }
                    Button {
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        "data-testid": "{test_id}-show-all",
                        // The button goes away with the hint; focus returns to
                        // the search so the dialog keeps it (and Escape works).
                        onclick: move |_| {
                            show_all.set(true);
                            focus_by_id(&search_focus);
                        },
                        "Show all versions"
                    }
                }
            } else if no_matches {
                p { class: "model-picker__empty", "No models match “{current_query}”." }
            }
            div { id: results_id, class: "model-picker__results", role: "listbox", "aria-label": "Models",
            for (group, rows) in groups {
                // A blank group is one the surrounding view already names.
                if !group.is_empty() {
                    SectionTitle { "{group}" }
                }
                List { class: "model-picker__group",
                    for option in rows {
                        {
                            let is_selected = selected.as_deref() == Some(option.selector.as_str());
                            let choice = option.selector.clone();
                            let option_id = format!("{test_id}-option-{row_index}");
                            let is_highlighted = row_index == highlighted;
                            row_index += 1;
                            rsx! {
                                ListRow {
                                    headline: rsx! { "{option.name}" },
                                    support: option.detail.as_ref().map(|detail| rsx! { "{detail}" }),
                                    trailing: is_selected.then(|| rsx! { Icon { name: "check".to_owned() } }),
                                    id: option_id,
                                    role: "option",
                                    aria_selected: is_highlighted.to_string(),
                                    tabindex: Some(-1),
                                    selected: is_highlighted,
                                    test_id: format!("model-picker-option-{}", option.selector),
                                    onclick: move |_| on_choose.call(choice.clone()),
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

fn scroll_choice_into_view(id: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
    {
        element.scroll_into_view_with_bool(false);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = id;
}
