//! OMP slash completions come from ACP command and mode catalogs.
//! This module adapts those owned wire values to shared menu rows and acceptance.

use super::composer_helpers::{fill_draft, focus_field};
use super::slash_menu::{
    SlashAccept, SlashMenuState, SlashOption, accept_action, slash_menu_state_for_omp,
};
use crate::pump::Pump;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog, OmpSlashCommand};
use std::rc::Rc;

pub(super) fn omp_menu_state(
    body: &str,
    modes: &[String],
    commands: &[OmpSlashCommand],
    sources: &super::slash_menu::SlashSources,
) -> Option<SlashMenuState> {
    let separated = body.split_once(char::is_whitespace);
    let (name, rest) = separated.unwrap_or((body, ""));
    let argument = rest.trim();
    if separated.is_some() {
        let options = match name {
            "model" => {
                super::model_hub::hub_logic::model_argument_values(&sources.models, argument)
                    .into_iter()
                    .map(|(value, summary)| option(name, summary, value))
                    .collect()
            }
            "effort" => sources
                .effort_levels
                .iter()
                .filter(|value| value.starts_with(argument))
                .map(|value| option(name, String::new(), value.clone()))
                .collect(),
            _ => Vec::new(),
        };
        return (!options.is_empty()).then_some(SlashMenuState {
            options,
            argument_command: None,
        });
    }
    let mut options = commands
        .iter()
        .filter(|command| {
            command.name.starts_with(name)
                && !(command.name == "plan" && modes.iter().any(|mode| mode == "plan"))
        })
        .map(|command| SlashOption {
            command_name: command.name.clone(),
            text: format!("/{}", command.name),
            needs_value: false,
            label: command.name.clone(),
            usage: command.input_hint.clone().unwrap_or_default(),
            summary: command.description.clone(),
        })
        .collect::<Vec<_>>();
    if "plan".starts_with(name) && modes.iter().any(|mode| mode == "plan") {
        options.push(SlashOption {
            command_name: "plan".to_owned(),
            text: "/plan".to_owned(),
            needs_value: false,
            label: "plan".to_owned(),
            usage: String::new(),
            summary: "Toggle plan mode".to_owned(),
        });
    }
    (!options.is_empty()).then_some(SlashMenuState {
        options,
        argument_command: None,
    })
}

pub(super) struct SlashAcceptance {
    pub native: bool,
    pub sources: super::slash_menu::SlashSources,
    pub commands: Vec<OmpSlashCommand>,
    pub modes: Vec<String>,
    pub draft: Signal<String>,
    pub selected_option: Signal<usize>,
    pub field: Signal<Option<Rc<MountedData>>>,
    pub send: EventHandler<()>,
}

pub(super) fn acceptance_handler(context: SlashAcceptance) -> EventHandler<usize> {
    let SlashAcceptance {
        native,
        sources,
        commands,
        modes,
        mut draft,
        selected_option,
        field,
        send,
    } = context;
    EventHandler::new(move |index| {
        let current = draft.peek().clone();
        let Some(option) = slash_menu_state_for_omp(
            &current,
            &sources,
            (!native).then_some(modes.as_slice()),
            &commands,
        )
        .and_then(|state| state.options.get(index).cloned()) else {
            return;
        };
        match accept_action(&option) {
            SlashAccept::Fill(text) => {
                fill_draft(draft, selected_option, &text);
                focus_field(field);
            }
            SlashAccept::Submit(text) => {
                draft.set(text);
                send.call(());
            }
        }
    })
}

fn option(command: &str, summary: String, value: String) -> SlashOption {
    SlashOption {
        command_name: command.to_owned(),
        text: format!("/{command} {value}"),
        needs_value: false,
        label: value,
        usage: String::new(),
        summary,
    }
}

pub(super) fn use_slash_sources(
    conversation: &ConversationSummary,
    catalog: Option<&ModelsCatalog>,
    pump: Pump,
    active: bool,
) -> (super::slash_menu::SlashSources, Vec<String>) {
    let mut sources = super::slash_menu::slash_sources(conversation, catalog);
    let omp = use_omp_catalog(
        pump,
        conversation.id.clone(),
        conversation.worker_fp.clone(),
        active && !conversation.is_native(),
    );
    let mut modes = Vec::new();
    if !conversation.is_native()
        && let Some(omp) = omp
    {
        modes = omp.modes.into_iter().map(|choice| choice.value).collect();
        sources.models = omp
            .models
            .into_iter()
            .map(|choice| (choice.value, choice.name))
            .collect();
        sources.effort_levels = omp
            .thinking_levels
            .into_iter()
            .map(|choice| choice.value)
            .collect();
    }
    (sources, modes)
}

fn use_omp_catalog(
    pump: Pump,
    conversation_id: String,
    worker_fp: String,
    active: bool,
) -> Option<roost_protocol::wire::agent_chat::OmpWorkerCatalog> {
    let _revision = pump.revision()();
    let settings_seq = super::omp_controls::settings_revision(&pump, &conversation_id, &worker_fp);
    let settings = use_resource(use_reactive(
        (&conversation_id, &active, &settings_seq),
        move |(_, active, _)| {
            let pump = pump.clone();
            async move {
                if active {
                    super::omp_controls::fetch_settings(pump).await
                } else {
                    Ok(roost_protocol::wire::agent_chat::OmpSettingsView::default())
                }
            }
        },
    ));
    settings
        .read()
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .and_then(|settings| {
            settings
                .catalogs
                .iter()
                .find(|catalog| catalog.worker_fp == worker_fp)
        })
        .cloned()
}
