//! Draft-input event ownership for the agent composer.
//! Slash acceptance stays with the combobox while IME, stop, and send gates stay
//! aligned with the product composer state.

use std::rc::Rc;

use dioxus::html::input_data::keyboard_types::{Key, NamedKey};
use dioxus::prelude::*;
use libero::components::ComboboxState;

use super::super::composer_helpers::fill_draft;
use super::super::slash_menu::{SlashSources, initial_selection, slash_menu_state};

#[derive(Clone, Copy)]
pub(super) struct ComposerDraftHandlers {
    pub(super) mounted: EventHandler<MountedEvent>,
    pub(super) input: EventHandler<String>,
    pub(super) keydown: EventHandler<KeyboardEvent>,
    pub(super) submit: EventHandler<()>,
}

pub(super) fn composer_placeholder(
    disabled: bool,
    send_available: bool,
    running: bool,
    busy_mode: &str,
) -> &'static str {
    if disabled {
        "The agent host is offline"
    } else if !send_available {
        "Listening…"
    } else if running {
        match busy_mode {
            "follow_up" => "Add a follow-up…",
            "reject" => "This message will be rejected while busy",
            _ => "Steer the agent…",
        }
    } else {
        "Ask the agent, or type / for commands…"
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn composer_draft_handlers(
    draft: Signal<String>,
    sources: SlashSources,
    mut field: Signal<Option<Rc<MountedData>>>,
    combobox: ComboboxState,
    mut menu_dismissed: Signal<bool>,
    menu_open: bool,
    options_open: bool,
    selected_text: Option<String>,
    running: bool,
    send_available: bool,
    on_provisional_input: EventHandler<()>,
    on_stop: EventHandler<()>,
    on_send: EventHandler<()>,
) -> ComposerDraftHandlers {
    let mounted = EventHandler::new(move |event: MountedEvent| field.set(Some(event.data())));
    let input_sources = sources.clone();
    let input = EventHandler::new(move |value: String| {
        on_provisional_input.call(());
        let next = slash_menu_state(&value, &input_sources);
        if let Some(next) = next.as_ref() {
            combobox.set_active(Some(initial_selection(next, &value)));
            combobox.open();
        } else {
            combobox.close();
        }
        menu_dismissed.set(false);
    });
    let keydown = EventHandler::new(move |event: KeyboardEvent| {
        if event.key() == Key::Named(NamedKey::Enter)
            && (event.modifiers().shift() || !send_available)
        {
            event.stop_propagation();
            return;
        }
        let key = event.key().to_string();
        if menu_open && key == "Escape" {
            event.prevent_default();
            event.stop_propagation();
            menu_dismissed.set(true);
            combobox.close();
            return;
        }
        if running && key == "Escape" {
            event.prevent_default();
            event.stop_propagation();
            on_stop.call(());
            return;
        }
        if options_open && key == "Tab" {
            event.prevent_default();
            if let Some(text) = &selected_text {
                fill_draft(draft, text);
                combobox.close();
            }
        }
    });
    let submit = EventHandler::new(move |()| {
        if !(options_open && combobox.is_open()) {
            on_send.call(());
        }
    });
    ComposerDraftHandlers {
        mounted,
        input,
        keydown,
        submit,
    }
}
