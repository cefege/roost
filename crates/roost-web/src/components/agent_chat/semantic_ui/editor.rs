//! Native multiline and single-line editors rendered with stock HTML controls.
//! Text and caret acknowledgments share the picker-query synchronization path;
//! host shortcuts still run through OMP's original keyboard decoder.
//! A masked input keeps its secret in this component and only sends edits.
use super::native_text::{NativeTextSnapshot, node_edit_ack, use_native_text};
use super::text_edit::current_value;
use crate::components::ui::{Input, InputType, Textarea};
use dioxus::prelude::*;
use serde_json::{Value, json};
use std::cell::Cell;
use std::rc::Rc;

#[component]
pub(super) fn OmpEditor(
    id: String,
    surface: String,
    text: String,
    readonly: bool,
    sendable: bool,
    typeahead: bool,
    focused: bool,
    cursor: Option<usize>,
    anchor: Option<usize>,
    placeholder: String,
    prompt: Option<String>,
    ghost: Option<String>,
    #[props(default = false)] single_line: bool,
    #[props(default = 18)] max_lines: usize,
    #[props(default = false)] secret: bool,
    #[props(optional)] edit_ack: Option<Value>,
    send: EventHandler<Value>,
) -> Element {
    let ack = edit_ack.as_ref().and_then(node_edit_ack);
    let control = use_native_text(
        NativeTextSnapshot {
            id: &id,
            surface: &surface,
            text: &text,
            cursor,
            anchor,
            focused,
            readonly,
            typeahead,
            mask: secret,
            ack,
            registration: "",
        },
        send,
    );
    let value = control.value();
    let blocked = control.awaiting_send();
    let html_readonly = readonly || blocked;
    let element_id = control.element_id().to_owned();
    let mut edit_control = control.clone();
    let composing = use_hook(|| Rc::new(Cell::new(false)));
    let input_composing = composing.clone();
    let on_input = EventHandler::new(move |event: FormEvent| {
        if !input_composing.get() {
            edit_control.edit(event.value());
        }
    });
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
    let mut key_control = control.clone();
    let key_id = id.clone();
    let key_surface = surface.clone();
    let on_key = EventHandler::new(move |event: KeyboardEvent| {
        if event.is_composing() {
            return;
        }
        let key = event.key();
        let modifiers = event.modifiers();
        if blocked {
            let clipboard = (modifiers.ctrl() || modifiers.meta())
                && matches!(&key,Key::Character(value) if matches!(value.as_str(),"a"|"c"|"v"|"x"));
            if !clipboard && let Some(data) = super::keyboard::input_data(&event) {
                event.prevent_default();
                event.stop_propagation();
                key_control.queue_input(data);
            }
            return;
        }
        if readonly {
            if !super::keyboard::browser_editing_key(
                &event,
                &key_control.value(),
                key_control.cursor(),
            ) && let Some(data) = super::keyboard::input_data(&event)
            {
                event.prevent_default();
                event.stop_propagation();
                send.call(json!({"method":"input","params":{"data":data}}));
            }
            return;
        }
        if (modifiers.ctrl() || modifiers.meta())
            && !modifiers.shift()
            && key == Key::Character("z".into())
        {
            event.prevent_default();
            event.stop_propagation();
            send.call(super::event("undo", &key_surface, &key_id, json!({})));
        } else if sendable
            && key == Key::Named(NamedKey::Enter)
            && !modifiers.shift()
            && !modifiers.alt()
        {
            event.prevent_default();
            event.stop_propagation();
            key_control.submit();
        } else if key == Key::Named(NamedKey::Enter)
            && !single_line
            && !modifiers.ctrl()
            && !modifiers.meta()
            && !modifiers.alt()
        {
        } else if !super::keyboard::browser_editing_key(
            &event,
            &key_control.value(),
            key_control.cursor(),
        ) && let Some(data) = super::keyboard::input_data(&event)
        {
            event.prevent_default();
            event.stop_propagation();
            key_control.queue_input(data);
        }
    });
    let mut paste_control = control.clone();
    let on_paste = EventHandler::new(move |event: ClipboardEvent| {
        if !html_readonly {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        {
            use dioxus::web::WebEventExt;
            let text = event
                .try_as_web_event()
                .and_then(|native: web_sys::ClipboardEvent| native.clipboard_data())
                .and_then(|data| data.get_data("text").ok())
                .unwrap_or_default();
            if text.is_empty() {
                return;
            }
            event.prevent_default();
            event.stop_propagation();
            paste_control.paste(text);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (&event, &mut paste_control);
        }
    });
    let label = prompt.clone().unwrap_or_else(|| "Message OMP".into());
    let max_lines = max_lines.clamp(1, 128);
    let placeholder = control.masked_placeholder().unwrap_or(placeholder);
    let input_type = if secret {
        InputType::Password
    } else {
        InputType::Text
    };
    let autocomplete = if secret { Some("off".to_owned()) } else { None };
    let spellcheck = if secret {
        Some("false".to_owned())
    } else {
        None
    };
    rsx! {div{class:"flex w-full min-w-0 flex-1 flex-col gap-2 pointer-events-auto",style:"--omp-native-editor-height:{max_lines}lh",
        onfocusin:move |_|if !focused{send.call(super::event("focus",&surface,&id,json!({})));},
        onpaste:on_paste,
        oncompositionstart:on_composition_start,
        oncompositionend:on_composition_end,
        if let Some(prompt)=prompt{label{r#for:element_id.clone(),class:"text-xs text-muted-foreground","{prompt}"}}
        if let Some(ghost)=ghost{if value.is_empty(){p{class:"text-xs text-muted-foreground",aria_hidden:"true","{ghost}"}}}
        if single_line{
            Input{id:Some(element_id),r#type:input_type,autocomplete,spellcheck,class:Some("w-full".into()),value:Some(value),placeholder:Some(placeholder),readonly:html_readonly,autofocus:focused,aria_label:Some(label),oninput:on_input,onkeydown:on_key}
        }else{
            Textarea{id:Some(element_id),class:Some("w-full resize-none max-h-(--omp-native-editor-height)".into()),rows:1,value:Some(value),placeholder:Some(placeholder),readonly:html_readonly,autofocus:focused,aria_label:Some(label),oninput:on_input,onkeydown:on_key}
        }
    }}
}
