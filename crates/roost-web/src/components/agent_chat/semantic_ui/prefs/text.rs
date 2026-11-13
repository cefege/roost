//! The open preference text field uses the native edit/ACK lane.
//! OMP supplies the draft and caret and owns commit, cancel and validation.
//! Masked rows retain typed plaintext locally without reading mask cells back.
use super::super::native_text::{NativeTextSnapshot, node_edit_ack, use_native_text};
use super::super::text_edit::current_value;
use crate::components::ui::{Input, InputType};
use dioxus::prelude::*;
use serde_json::Value;
use std::cell::Cell;
use std::rc::Rc;

#[component]
#[allow(clippy::too_many_arguments)]
pub(super) fn PrefText(
    id: String,
    surface: String,
    text: String,
    cursor: Option<usize>,
    ack: Option<Value>,
    placeholder: Option<String>,
    label: String,
    disabled: bool,
    secret: bool,
    mono: bool,
    send: EventHandler<Value>,
) -> Element {
    let ack = ack.as_ref().and_then(node_edit_ack);
    let control = use_native_text(
        NativeTextSnapshot {
            id: &id,
            surface: &surface,
            text: &text,
            cursor,
            anchor: None,
            focused: true,
            readonly: disabled,
            typeahead: false,
            mask: secret,
            ack,
            registration: "prefs-row",
        },
        send,
    );
    let value = control.value();
    let element_id = control.element_id().to_owned();
    let mut edit_control = control.clone();
    let composing = use_hook(|| Rc::new(Cell::new(false)));
    let input_composing = composing.clone();
    let composition_id = element_id.clone();
    let mut composition_control = control.clone();
    let start_composing = composing.clone();
    let on_composition_start = move |_event: CompositionEvent| start_composing.set(true);
    let on_composition_end = move |_event: CompositionEvent| {
        composing.set(false);
        if let Some(value) = current_value(&composition_id) {
            composition_control.edit(value);
        }
    };
    let placeholder = control.masked_placeholder().or(placeholder);
    let mut key_control = control;
    let on_key = move |event: KeyboardEvent| {
        if event.is_composing() || disabled {
            return;
        }
        if matches!(event.key().to_string().as_str(), "Backspace" | "Delete")
            && key_control.clear_unknown_secret()
        {
            event.prevent_default();
            return;
        }
        if !super::super::keyboard::browser_editing_key(
            &event,
            &key_control.value(),
            key_control.cursor(),
        ) && let Some(data) = super::super::keyboard::input_data(&event)
        {
            event.prevent_default();
            event.stop_propagation();
            key_control.queue_input(data);
        }
    };
    rsx! {
        div {
            class: "w-full max-w-xs pointer-events-auto",
            oncompositionstart: on_composition_start,
            oncompositionend: on_composition_end,
            Input {
                id: Some(element_id),
                r#type: if secret { InputType::Password } else { InputType::Text },
                autocomplete: secret.then(|| "off".into()),
                spellcheck: (secret || mono).then(|| "false".into()),
                class: Some(if mono { "w-full font-mono" } else { "w-full" }.into()),
                value: Some(value),
                placeholder,
                readonly: disabled,
                autofocus: true,
                aria_label: Some(label),
                oninput: move |event: FormEvent| if !input_composing.get() { edit_control.edit(event.value()); },
                onkeydown: on_key,
            }
        }
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
