//! The slash-command menu under the agent composer's field. `slash_menu_state`
//! decides what a draft can complete to, from the shared registry: command
//! names while the draft is one word, then the subcommands or argument values
//! of the single command it names. `SlashMenu` renders that as OMP's list does:
//! a name column sized to the widest visible name, then usage and summary.
//! The composer owns the draft and the keys; `accept_action` decides Enter.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AGENT_SLASH_COMMANDS, ConversationSummary, ModelsCatalog, SlashCommand,
};

use super::model_controls::supports_reasoning;
use super::model_hub::hub_logic::model_argument_values;
use crate::components::md::{List, ListRow};

#[cfg(target_arch = "wasm32")]
use web_sys::{ScrollIntoViewOptions, ScrollLogicalPosition};

/// The name column's bounds in characters, OMP's slash-list layout.
const NAME_COLUMN_MIN_CHARS: usize = 12;
const NAME_COLUMN_MAX_CHARS: usize = 32;
/// Space between the widest name and the summary column.
const NAME_COLUMN_GAP_CHARS: usize = 2;

/// One completion the menu offers.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SlashOption {
    pub(super) command: &'static SlashCommand,
    /// The draft this option produces. A trailing space means more input is expected.
    pub(super) text: String,
    /// Enter cannot run `text` as it stands: a value must be typed after it.
    pub(super) needs_value: bool,
    /// What the name column shows: the command name, or the argument value.
    pub(super) label: String,
    /// The command's usage, shown ahead of the summary on name rows only.
    pub(super) usage: &'static str,
    pub(super) summary: String,
}

/// What the menu shows for a draft.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SlashMenuState {
    pub(super) options: Vec<SlashOption>,
    /// The command the draft has named past its name. With nothing left to
    /// complete, its usage is the one line the menu shows.
    pub(super) argument_command: Option<&'static SlashCommand>,
}

/// What Enter on an option does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SlashAccept {
    /// Put the text in the field and keep typing: the command needs an argument.
    Fill(String),
    /// Run the text now.
    Submit(String),
}

/// `None` when the draft is not a command, or names a command and has moved past
/// its one argument word (typed text is the harness's, not a completion).
pub(super) fn slash_menu_state(draft: &str, sources: &SlashSources) -> Option<SlashMenuState> {
    let body = draft.strip_prefix('/')?;
    if draft.contains('\n') {
        return None;
    }
    let Some((name, rest)) = body.split_once(char::is_whitespace) else {
        let options = name_options(&body.to_ascii_lowercase());
        return (!options.is_empty()).then_some(SlashMenuState {
            options,
            argument_command: None,
        });
    };
    let command = AGENT_SLASH_COMMANDS
        .iter()
        .find(|command| command.name == name || command.aliases.contains(&name))?;
    let argument = rest.trim_start();
    if argument.contains(char::is_whitespace) {
        return None;
    }
    Some(SlashMenuState {
        options: argument_options(command, &argument.to_ascii_lowercase(), sources),
        argument_command: Some(command),
    })
}

/// The option the draft already matches, else the first one.
pub(super) fn initial_selection(state: &SlashMenuState, draft: &str) -> usize {
    let typed = draft.trim();
    state
        .options
        .iter()
        .position(|option| option.text.trim_end() == typed)
        .unwrap_or(0)
}

/// Enter runs the chosen command, as OMP's does, unless it cannot run without
/// a value; then the text is completed and the value is typed next.
pub(super) fn accept_action(option: &SlashOption) -> SlashAccept {
    if option.needs_value {
        SlashAccept::Fill(option.text.clone())
    } else {
        SlashAccept::Submit(option.text.trim_end().to_string())
    }
}

/// The shared name-column width: the widest visible label plus a gap,
/// clamped so one long model id cannot push the summaries off the row.
pub(super) fn name_column_chars(options: &[SlashOption]) -> usize {
    let widest = options
        .iter()
        .map(|option| option.label.chars().count())
        .max()
        .unwrap_or(0);
    (widest + NAME_COLUMN_GAP_CHARS).clamp(NAME_COLUMN_MIN_CHARS, NAME_COLUMN_MAX_CHARS)
}

/// Argument values that come from the conversation rather than the registry.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SlashSources {
    /// `/effort`: the levels the conversation's model accepts.
    pub(super) effort_levels: Vec<String>,
    /// `/model` and `/switch`: available chat models as `(selector, name)`.
    pub(super) models: Vec<(String, String)>,
}

pub(super) fn slash_sources(
    conversation: &ConversationSummary,
    catalog: Option<&ModelsCatalog>,
) -> SlashSources {
    let Some(catalog) = catalog else {
        return SlashSources::default();
    };
    let effort_levels = if supports_reasoning(conversation, Some(catalog)) {
        catalog.thinking_levels.clone()
    } else {
        Vec::new()
    };
    let models = catalog
        .models
        .iter()
        .filter(|model| model.available && !model.classifier)
        .map(|model| {
            (
                format!("{}/{}", model.provider, model.model_id),
                model.name.clone(),
            )
        })
        .collect();
    SlashSources {
        effort_levels,
        models,
    }
}

fn name_options(partial: &str) -> Vec<SlashOption> {
    let mut ranked: Vec<(u8, &'static SlashCommand)> = AGENT_SLASH_COMMANDS
        .iter()
        .filter_map(|command| name_rank(command, partial).map(|rank| (rank, command)))
        .collect();
    // Stable: equal ranks keep the registry's order.
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked
        .into_iter()
        .map(|(_, command)| SlashOption {
            command,
            text: if command.args.is_empty() {
                format!("/{}", command.name)
            } else {
                format!("/{} ", command.name)
            },
            needs_value: command.requires_argument() && !command.client_bare,
            label: command.name.to_string(),
            usage: command.args,
            summary: command.summary.to_string(),
        })
        .collect()
}

/// Lower rank sorts first: exact name, name prefix, alias prefix, the letters
/// in order anywhere in the name, then a summary match for two letters or more.
fn name_rank(command: &SlashCommand, partial: &str) -> Option<u8> {
    if partial.is_empty() || command.name == partial {
        return Some(0);
    }
    if command.name.starts_with(partial) {
        return Some(1);
    }
    if command
        .aliases
        .iter()
        .any(|alias| alias.starts_with(partial))
    {
        return Some(2);
    }
    if is_subsequence(partial, command.name) {
        return Some(3);
    }
    (partial.len() >= 2 && command.summary.to_ascii_lowercase().contains(partial)).then_some(4)
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut remaining = haystack.chars();
    needle
        .chars()
        .all(|wanted| remaining.any(|candidate| candidate == wanted))
}

fn argument_options(
    command: &'static SlashCommand,
    partial: &str,
    sources: &SlashSources,
) -> Vec<SlashOption> {
    let candidates: Vec<(String, String, bool)> = match command.name {
        "effort" => sources
            .effort_levels
            .iter()
            .filter(|level| level.starts_with(partial))
            .map(|level| (level.clone(), String::new(), false))
            .collect(),
        // Already filtered and ranked by the fuzzy model search.
        "model" | "switch" => model_argument_values(&sources.models, partial)
            .into_iter()
            .map(|(value, summary)| (value, summary, false))
            .collect(),
        _ => command
            .subcommands
            .iter()
            .filter(|argument| argument.name.starts_with(partial))
            .map(|argument| {
                (
                    argument.name.to_string(),
                    argument.summary.to_string(),
                    argument.takes_value,
                )
            })
            .collect(),
    };
    candidates
        .into_iter()
        .map(|(value, summary, needs_value)| SlashOption {
            command,
            text: if needs_value {
                format!("/{} {value} ", command.name)
            } else {
                format!("/{} {value}", command.name)
            },
            needs_value,
            label: value,
            usage: "",
            summary,
        })
        .collect()
}

/// The listbox id for a conversation's composer.
pub(super) fn listbox_id(conversation_id: &str) -> String {
    format!("agent-slash-list-{}", sanitize_id(conversation_id))
}

/// The id of one option row, used for `aria-activedescendant` and scrolling.
pub(super) fn option_id(conversation_id: &str, index: usize) -> String {
    format!("agent-slash-{}-{index}", sanitize_id(conversation_id))
}

fn sanitize_id(conversation_id: &str) -> String {
    conversation_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

#[component]
pub fn SlashMenu(
    conversation_id: String,
    state: SlashMenuState,
    selected: usize,
    on_choose: EventHandler<usize>,
) -> Element {
    let listbox = listbox_id(&conversation_id);
    let name_chars = name_column_chars(&state.options);
    // With nothing left to complete, the named command's usage is all there is
    // to say; while a list is open its rows already carry it.
    let usage_line = state.argument_command.filter(|_| state.options.is_empty());
    rsx! {
        div { class: "agent-chat__slash-menu", "data-testid": "agent-chat-command-menu",
            onmousedown: move |event: MouseEvent| event.prevent_default(),
            if let Some(command) = usage_line {
                p { class: "agent-chat__slash-usage", "data-testid": "agent-chat-command-usage",
                    span { class: "agent-chat__slash-args",
                        "/{command.name}"
                        if !command.args.is_empty() {
                            " {command.args}"
                        }
                    }
                    " {command.summary}"
                }
            } else {
                div { id: "{listbox}", role: "listbox", aria_label: "Commands",
                    style: "--slash-name-chars: {name_chars}",
                    List {
                        for (idx, option) in state.options.iter().enumerate() {
                            ListRow {
                                key: "{idx}-{option.text}",
                                id: Some(option_id(&conversation_id, idx)),
                                role: Some("option".to_string()),
                                aria_selected: Some(if idx == selected { "true" } else { "false" }.to_string()),
                                tabindex: Some(-1),
                                test_id: Some(format!("agent-slash-option-{idx}")),
                                class: Some("agent-chat__slash-row".to_string()),
                                headline: rsx! { span { class: "agent-chat__slash-name", "{option.label}" } },
                                support: (!option.usage.is_empty() || !option.summary.is_empty()).then(|| rsx! {
                                    span {
                                        if !option.usage.is_empty() {
                                            span { class: "agent-chat__slash-args", "{option.usage}" }
                                            " "
                                        }
                                        "{option.summary}"
                                    }
                                }),
                                selected: idx == selected,
                                dense: true,
                                onclick: EventHandler::new(move |_| on_choose.call(idx)),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Keeps the keyboard-selected option in the menu's scroll area. Scrolls to the
/// nearest edge so the composer and page stay put.
pub(super) fn scroll_option_into_view(conversation_id: &str, index: usize) {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(&option_id(conversation_id, index)))
        else {
            return;
        };
        let options = ScrollIntoViewOptions::new();
        options.set_block(ScrollLogicalPosition::Nearest);
        element.scroll_into_view_with_scroll_into_view_options(&options);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (conversation_id, index);
}

#[cfg(test)]
#[path = "slash_menu_tests.rs"]
mod tests;
