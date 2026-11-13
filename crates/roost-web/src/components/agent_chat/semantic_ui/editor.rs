//! Native multiline and single-line editors rendered with stock HTML controls.
//! Text and caret acknowledgments share the picker-query synchronization path;
//! host shortcuts still run through OMP's original keyboard decoder.
//! Enter remains a decoded key intent; atomic `send` supplies a separate prompt.
//! A masked input keeps its secret in this component and only sends edits.
use super::native_text::{NativeTextSnapshot, node_edit_ack, use_native_text};
use super::text_edit::current_value;
use crate::components::ui::{Badge, Input, InputType, Textarea};
use dioxus::prelude::*;
use serde_json::{Value, json};
use std::cell::Cell;
use std::rc::Rc;

#[path = "editor_attachments.rs"]
mod attachments;
#[path = "editor_chrome.rs"]
mod chrome;
#[path = "editor_drawing.rs"]
mod drawing;
#[path = "editor_passthrough.rs"]
mod passthrough;

#[component]
pub(super) fn OmpEditor(
    id: String,
    surface: String,
    text: String,
    readonly: bool,
    typeahead: bool,
    focused: bool,
    cursor: Option<usize>,
    anchor: Option<usize>,
    placeholder: Value,
    prompt: Option<Value>,
    ghost: Option<String>,
    #[props(default = false)] single_line: bool,
    #[props(optional)] decor: Option<Value>,
    #[props(optional)] language: Option<String>,
    #[props(optional)] chrome: Option<Value>,
    #[props(optional)] mode: Option<String>,
    #[props(default = 18)] max_lines: usize,
    #[props(default = false)] secret: bool,
    #[props(optional)] edit_ack: Option<Value>,
    send: EventHandler<Value>,
) -> Element {
    let ack = edit_ack.as_ref().and_then(node_edit_ack);
    let raw_input = mode.as_deref().is_some_and(|mode| !mode.is_empty());
    let pty_input = mode.as_deref() == Some("PTY");
    let control = use_native_text(
        NativeTextSnapshot {
            id: &id,
            surface: &surface,
            text: &text,
            cursor,
            anchor,
            focused,
            readonly: readonly || raw_input,
            typeahead,
            mask: secret,
            ack,
            registration: "",
        },
        send,
    );
    let value = control.value();
    let html_readonly = readonly;
    let element_id = control.element_id().to_owned();
    let raw_unavailable =
        passthrough::use_input(&element_id, raw_input, &value, control.cursor(), send);
    let uploads = attachments::use_attachments(
        &element_id,
        &surface,
        &id,
        !single_line && !secret && !readonly && !raw_input,
    );
    let mut edit_control = control.clone();
    let composing = use_hook(|| Rc::new(Cell::new(false)));
    let input_composing = composing.clone();
    let on_input = EventHandler::new(move |event: FormEvent| {
        if !raw_input && !input_composing.get() {
            edit_control.edit(event.value());
        }
    });
    let composition_id = element_id.clone();
    let composition_start_id = element_id.clone();
    let mut composition_control = control.clone();
    let start_composing = composing.clone();
    let on_composition_start = move |_event: CompositionEvent| {
        start_composing.set(true);
        super::text_edit::mark_composing(&composition_start_id, true);
    };
    let on_composition_end = move |_event: CompositionEvent| {
        composing.set(false);
        super::text_edit::mark_composing(&composition_id, false);
        if !raw_input && let Some(value) = current_value(&composition_id) {
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
        if raw_input {
            let clipboard = pty_input
                && modifiers.meta()
                && matches!(&key, Key::Character(value) if matches!(value.as_str(), "a" | "c" | "v" | "x"));
            if !clipboard && let Some(data) = super::keyboard::input_data(&event) {
                event.prevent_default();
                event.stop_propagation();
                passthrough::emit(data, send);
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
        if !html_readonly && !raw_input {
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
            if raw_input {
                passthrough::emit(format!("\u{1b}[200~{text}\u{1b}[201~"), send);
            } else {
                paste_control.paste(text);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (&event, &mut paste_control);
        }
    });
    let label = prompt
        .as_ref()
        .map(super::text_value)
        .unwrap_or_else(|| "Message OMP".into());
    let masked_drawing = secret && (decor.is_some() || ghost.is_some() || language.is_some());
    let drawing_placeholder = (!secret && placeholder.is_array()).then(|| placeholder.clone());
    let native_drawing = !secret && value == text;
    let drawing_ghost = native_drawing.then_some(ghost).flatten();
    let drawing_decor = native_drawing.then_some(decor).flatten();
    let draw = !secret
        && (drawing_ghost.is_some()
            || drawing_decor.is_some()
            || language.is_some()
            || drawing_placeholder.is_some());
    let max_lines = max_lines.clamp(1, 128);
    let placeholder = control
        .masked_placeholder()
        .unwrap_or_else(|| super::text_value(&placeholder));
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
    let shaped = chrome.is_some();
    let textarea_class = if shaped {
        "block w-full resize-none self-start max-h-(--omp-native-editor-height) rounded-none border-0 bg-transparent dark:bg-transparent p-0 font-mono leading-normal shadow-none min-h-lh focus-visible:border-0 focus-visible:ring-0"
    } else {
        "w-full resize-none max-h-(--omp-native-editor-height) rounded-none border-0 bg-transparent dark:bg-transparent px-0 py-1 font-mono shadow-none min-h-8 focus-visible:border-0 focus-visible:ring-0"
    };
    let mode_attribute = mode.clone();
    rsx! {div{class:"flex w-full min-w-0 flex-1 flex-col gap-2 pointer-events-auto",style:"--omp-native-editor-height:{max_lines}lh",
        "data-omp-native-editor":"true",
        "data-omp-editor-mode":mode_attribute,
        onfocusin:move |_|if !focused{send.call(super::event("focus",&surface,&id,json!({})));},
        onpaste:on_paste,
        oncompositionstart:on_composition_start,
        oncompositionend:on_composition_end,
        if let Some(prompt)=prompt{label{r#for:element_id.clone(),class:"text-xs text-muted-foreground",
            {super::content::render_spans(Some(&prompt), "")}
        }}
        if let Some(mode)=mode{div{class:"flex items-center",Badge{"{mode}"}}}
        if masked_drawing{p{role:"alert",class:"text-xs text-destructive","OMP decorations and completions are unavailable for masked fields."}}
        if raw_unavailable(){p{role:"alert",class:"text-xs text-destructive","OMP raw input could not attach to the native text control."}}
        chrome::ComposerChrome { chrome, element_id:element_id.clone(),
        div{class:"relative w-full min-w-0",
            if single_line{
                Input{id:Some(element_id.clone()),r#type:input_type,autocomplete,spellcheck,class:Some("w-full font-mono".into()),value:Some(value.clone()),placeholder:Some(placeholder),readonly:html_readonly,autofocus:focused,aria_label:Some(label),oninput:on_input,onkeydown:on_key}
            }else{
                Textarea{id:Some(element_id.clone()),class:Some(textarea_class.into()),rows:1,value:Some(value.clone()),placeholder:Some(placeholder),readonly:html_readonly,autofocus:focused,aria_label:Some(label),oninput:on_input,onkeydown:on_key}
            }
            if draw{
                drawing::EditorDrawing{element_id,text:value,cursor:control.cursor(),ghost:drawing_ghost,decor:drawing_decor,language,placeholder:drawing_placeholder}
            }
        }
        }
        attachments::AttachmentControls { owner: uploads, disabled: html_readonly }
    }}
}
