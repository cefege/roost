//! Native picker and settings queries using the stock search input.
//! Text acknowledgments share composer synchronization and cannot move a local
//! caret while a newer query is being typed or pasted.
use super::native_text::{NativeTextSnapshot, query_edit_ack, use_native_text};
use super::text_edit::current_value;
use crate::components::ui::{Input, InputType};
use dioxus::prelude::*;
use serde_json::Value;
use std::cell::Cell;
use std::rc::Rc;

#[component]
pub(super) fn NativeQuery(
    id: String,
    surface: String,
    text: String,
    cursor: Option<usize>,
    placeholder: String,
    #[props(default = true)] focused: bool,
    #[props(optional)] edit_ack: Option<Value>,
    send: EventHandler<Value>,
) -> Element {
    let ack = edit_ack.as_ref().and_then(query_edit_ack);
    let control = use_native_text(
        NativeTextSnapshot {
            id: &id,
            surface: &surface,
            text: &text,
            cursor,
            anchor: None,
            focused,
            readonly: false,
            typeahead: false,
            mask: false,
            ack,
            registration: "",
        },
        send,
    );
    let element_id = control.element_id().to_owned();
    let value = control.value();
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
    let mut key_control = control;
    rsx! {div{class:"w-full pointer-events-auto",oncompositionstart:on_composition_start,oncompositionend:on_composition_end,
        Input{
            id:Some(element_id),r#type:InputType::Search,class:Some("w-full pointer-events-auto".into()),value:Some(value),
            placeholder:Some(placeholder),aria_label:Some("Search".into()),autocomplete:Some("off".into()),
            oninput:move|event:FormEvent|if !input_composing.get(){edit_control.edit(event.value());},
            onkeydown:move|event:KeyboardEvent|{
                if event.is_composing(){return;}
                if !super::keyboard::browser_editing_key(&event,&key_control.value(),key_control.cursor())
                    && let Some(data)=super::keyboard::input_data(&event){event.prevent_default();event.stop_propagation();key_control.queue_input(data);}
            }
        }
    }}
}
