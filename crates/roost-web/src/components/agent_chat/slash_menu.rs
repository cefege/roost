//! The slash-command menu above the agent composer. `slash_menu_state` decides
//! what a draft can complete to, from the shared registry: command names while
//! the draft is one word, then the subcommands or argument values of the single
//! command it names. `SlashMenu` shows that as a listbox with the selected
//! command's usage and examples. The composer owns the draft and the keys.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AGENT_SLASH_COMMANDS, ConversationSummary, ModelsCatalog, SlashCommand,
};

use super::model_controls::supports_reasoning;
use super::model_hub::hub_logic::model_argument_values;
use crate::components::md::{List, ListRow};

#[cfg(target_arch = "wasm32")]
use web_sys::{ScrollIntoViewOptions, ScrollLogicalPosition};

/// One completion the menu offers.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SlashOption {
    pub(super) command: &'static SlashCommand,
    /// The draft this option produces. A trailing space means more input is expected.
    pub(super) text: String,
    pub(super) headline: String,
    pub(super) summary: String,
}

/// What the menu shows for a draft.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SlashMenuState {
    pub(super) options: Vec<SlashOption>,
    /// The command the draft has named past its name. Its usage and examples
    /// stay visible while the arguments are typed.
    pub(super) argument_command: Option<&'static SlashCommand>,
}

impl SlashMenuState {
    /// The command whose usage and examples the footer shows.
    pub(super) fn help_command(&self, selected: usize) -> Option<&'static SlashCommand> {
        self.argument_command
            .or_else(|| self.options.get(selected).map(|option| option.command))
    }
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
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked
        .into_iter()
        .map(|(_, command)| {
            let headline = if command.args.is_empty() {
                format!("/{}", command.name)
            } else {
                format!("/{} {}", command.name, command.args)
            };
            let text = if command.args.is_empty() {
                format!("/{}", command.name)
            } else {
                format!("/{} ", command.name)
            };
            SlashOption {
                command,
                text,
                headline,
                summary: command.summary.to_string(),
            }
        })
        .collect()
}

/// Lower rank sorts first: exact name, name prefix, alias prefix, then a
/// description match for words of two letters or more.
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
    (partial.len() >= 2 && command.summary.to_ascii_lowercase().contains(partial)).then_some(3)
}

fn argument_options(
    command: &'static SlashCommand,
    partial: &str,
    sources: &SlashSources,
) -> Vec<SlashOption> {
    let candidates: Vec<(String, String)> = match command.name {
        "effort" => sources
            .effort_levels
            .iter()
            .filter(|level| level.starts_with(partial))
            .map(|level| (level.clone(), String::new()))
            .collect(),
        // Already filtered and ranked by the fuzzy model search.
        "model" | "switch" => model_argument_values(&sources.models, partial),
        _ => command
            .subcommands
            .iter()
            .filter(|argument| argument.name.starts_with(partial))
            .map(|argument| (argument.name.to_string(), argument.summary.to_string()))
            .collect(),
    };
    candidates
        .into_iter()
        .map(|(value, summary)| SlashOption {
            command,
            text: format!("/{} {value}", command.name),
            headline: value,
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
    let help = state.help_command(selected);
    let listbox = listbox_id(&conversation_id);
    let has_options = !state.options.is_empty();
    rsx! {
        div { class: "agent-chat__slash-menu",
            onmousedown: move |event: MouseEvent| event.prevent_default(),
            div { class: "agent-chat__slash-panel", "data-testid": "agent-chat-command-menu",
            if let Some(command) = help {
                div { class: "agent-chat__command-help", "data-testid": "agent-chat-command-help",
                    div { class: "agent-chat__command-usage",
                        "/{command.name}"
                        if !command.args.is_empty() {
                            " {command.args}"
                        }
                    }
                    span { class: "agent-chat__command-summary", "{command.summary}" }
                    if !command.examples.is_empty() {
                        div { class: "agent-chat__command-examples",
                            for example in command.examples.iter() {
                                code { class: "agent-chat__command-example", "{example}" }
                            }
                        }
                    }
                }
            }
            if has_options {
                div { id: "{listbox}", role: "listbox", aria_label: "Commands",
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
                                headline: rsx! { span { class: "agent-chat__slash-name", "{option.headline}" } },
                                support: (!option.summary.is_empty()).then(|| rsx! { span { "{option.summary}" } }),
                                selected: idx == selected,
                                dense: true,
                                onclick: EventHandler::new(move |_| on_choose.call(idx)),
                            }
                        }
                    }
                }
                div { class: "agent-chat__command-hint", "↑↓ move · Tab complete · Enter select · Esc close" }
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
