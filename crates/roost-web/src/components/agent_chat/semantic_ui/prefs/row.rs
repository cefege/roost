//! Native preference rows and their owning prefs-node addresses.
//! The page renderer borrows each row; only event routing fields are retained.
//! Control changes are handled by OMP rather than local settings state.
use super::super::native_text::{NativeTextSnapshot, node_edit_ack, use_native_text};
use super::super::text_edit::current_value;
use super::super::{event, text_value};
use super::multi::multi_control;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Input, InputType, Select, SelectOption, Switch,
};
use dioxus::prelude::*;
use serde_json::{Value, json};
use std::cell::Cell;
use std::rc::Rc;

/// A secret row's open editor: the browser keeps the plaintext it typed and
/// sends one masked native edit at a time, so the draft never carries bullets.
#[component]
fn PrefSecretText(
    id: String,
    surface: String,
    text: String,
    ack: Option<Value>,
    placeholder: Option<String>,
    label: String,
    disabled: bool,
    send: EventHandler<Value>,
) -> Element {
    let ack = ack.as_ref().and_then(node_edit_ack);
    let control = use_native_text(
        NativeTextSnapshot {
            id: &id,
            surface: &surface,
            text: &text,
            cursor: None,
            anchor: None,
            focused: true,
            readonly: disabled,
            typeahead: false,
            mask: true,
            ack,
            registration: "secret-row",
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
        let key = event.key();
        if key == Key::Named(NamedKey::Escape) || key == Key::Named(NamedKey::Enter) {
            if let Some(data) = super::super::keyboard::input_data(&event) {
                event.prevent_default();
                event.stop_propagation();
                key_control.queue_input(data);
            }
        } else if !super::super::keyboard::browser_editing_key(
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
    rsx! {div{class:"w-full max-w-xs pointer-events-auto",oncompositionstart:on_composition_start,oncompositionend:on_composition_end,
        Input{
            id:Some(element_id),r#type:InputType::Password,autocomplete:Some("off".into()),spellcheck:Some("false".into()),
            class:Some("w-full".into()),value:Some(value),placeholder,readonly:disabled,autofocus:true,aria_label:Some(label),
            oninput:move|event:FormEvent|if !input_composing.get(){edit_control.edit(event.value());},
            onkeydown:on_key
        }
    }}
}
pub(super) fn pref_row(
    row: &Value,
    prefs_id: &str,
    surface: &str,
    editing: Option<&Value>,
    focused: Option<&str>,
    send: &EventHandler<Value>,
) -> Element {
    let send = *send;
    let row_id = row
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let label = row
        .get("label")
        .map(text_value)
        .unwrap_or_else(|| row_id.clone());
    let control = row.get("control").unwrap_or(&Value::Null);
    let kind = control.get("k").and_then(Value::as_str).unwrap_or("action");
    let disabled = row
        .get("disabled")
        .is_some_and(|value| !value.is_null() && value != false);
    let prefs_id = prefs_id.to_owned();
    let surface = surface.to_owned();
    let focused_now = focused == Some(row_id.as_str());
    let changed = row.get("changed").and_then(Value::as_bool).unwrap_or(false);
    let default_label = row
        .get("defaultLabel")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let hint = row.get("hint").and_then(Value::as_str).map(str::to_owned);
    let warning = row
        .get("warning")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let focus_id = prefs_id.clone();
    let focus_surface = surface.clone();
    let focus_send = send;
    rsx! {div{class:if focused_now{"flex flex-wrap items-center justify-between gap-2 border-b border-ring py-2"}else{"flex flex-wrap items-center justify-between gap-2 border-b border-border py-2"},onfocusin:move |_|focus_send.call(event("focus",&focus_surface,&focus_id,json!({}))),
        div{class:"min-w-0",p{class:"text-sm font-medium", "{label}" if changed {span{class:"ml-2 text-primary",title:default_label,"•"}}} if let Some(hint)=hint{p{class:"text-xs text-muted-foreground","{hint}"}} if let Some(warning)=warning{p{class:"text-xs text-warning","{warning}"}}}
        {match kind{
            "switch"=>{let value=control.get("on").and_then(Value::as_bool).unwrap_or(false);let event_id=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();rsx!{Switch{checked:value,disabled,aria_label:label.clone(),on_checked_change:move|value|send.call(event("change",&sf,&event_id,json!({"item":item,"value":value})))}}},
            "select"|"choice"=>{let value=control.get("value").and_then(Value::as_str).unwrap_or_default().to_owned();let options=control.get("options").and_then(Value::as_array);let event_id=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();if control.get("style").and_then(Value::as_str)==Some("segmented"){rsx!{crate::components::ui::Tabs{value,on_value_change:move|value|send.call(event("change",&sf,&event_id,json!({"item":item,"value":value}))),crate::components::ui::TabsList{aria_label:Some(label.clone()),variant:crate::components::ui::TabsVariant::Default,for option in options.into_iter().flatten(){ {let option_value=option.get("value").and_then(Value::as_str).unwrap_or_default().to_owned();let option_label=option.get("label").map(text_value).unwrap_or_else(||option_value.clone());rsx!{crate::components::ui::TabsTrigger{value:option_value,disabled,has_panel:false,"{option_label}"}}}}}}}}else{let options=options.into_iter().flatten().map(|option|SelectOption::new(option.get("value").and_then(Value::as_str).unwrap_or_default(),option.get("label").map(text_value).unwrap_or_default())).collect::<Vec<_>>();rsx!{Select{value,on_change:move|value|send.call(event("change",&sf,&event_id,json!({"item":item,"value":value}))),label:Some(label.clone()),options,disabled,class:Some("w-full max-w-xs".into())}}}},
            "number"=>{let value=control.get("value").map(text_value).unwrap_or_default();let min=control.get("min").map(text_value);let max=control.get("max").map(text_value);let step=control.get("step").map(text_value);let unit=control.get("unit").and_then(Value::as_str).map(str::to_owned);let display_label=control.get("labels").and_then(Value::as_object).and_then(|labels|labels.get(&value)).and_then(Value::as_str).map(str::to_owned);let sid=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();rsx!{div{class:"flex flex-wrap items-center gap-2",Input{r#type:InputType::Number,class:Some("w-32".into()),value:Some(value),min,max,step,disabled,aria_label:Some(label.clone()),onchange:move|input: FormEvent|{if let Ok(number)=input.value().parse::<f64>()&& number.is_finite(){send.call(super::event("change",&sf,&sid,json!({"item":item,"value":number})));}}}if let Some(unit)=unit{span{class:"text-sm text-muted-foreground","{unit}"}}if let Some(display_label)=display_label{span{class:"text-sm text-muted-foreground","{display_label}"}}}}},
            "keys"=>{let keys=control.get("keys").and_then(Value::as_array);let active=editing.and_then(|value|value.get("row")).and_then(Value::as_str)==Some(row_id.as_str());let display=keys.map(|keys|keys.iter().map(key_chord).collect::<Vec<_>>().join(", ")).unwrap_or_default();let button_label=if active{"Press a key…"}else if display.is_empty(){"Set shortcut"}else{display.as_str()};let sid=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();rsx!{Button{size:ButtonSize::Sm,variant:if active{ButtonVariant::Secondary}else{ButtonVariant::Outline},disabled,onclick:move |_|send.call(event("action",&sf,&sid,json!({"act":"edit","value":item}))),"{button_label}"}}},
            "text"=>{let secret=control.get("secret").and_then(Value::as_bool).unwrap_or(false);let active=editing.filter(|value|value.get("row").and_then(Value::as_str)==Some(row_id.as_str()));if secret&&active.is_some(){let draft=active.and_then(|value|value.get("draft")).and_then(Value::as_str).unwrap_or_default().to_owned();let ack=active.and_then(|value|value.get("ack")).cloned();let placeholder=control.get("placeholder").and_then(Value::as_str).map(str::to_owned);let secret_id=prefs_id.clone();let secret_surface=surface.clone();let secret_row=row_id.clone();let secret_label=label.clone();rsx!{PrefSecretText{key:"{secret_row}",id:secret_id,surface:secret_surface,text:draft,ack,placeholder,label:secret_label,disabled,send}}}else{let value=active.and_then(|value|value.get("draft")).and_then(Value::as_str).map(str::to_owned).unwrap_or_else(||if secret{String::new()}else{control.get("value").and_then(Value::as_str).unwrap_or_default().to_owned()});let placeholder=control.get("placeholder").and_then(Value::as_str).map(str::to_owned);let sid=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();let escape_send=send;rsx!{Input{r#type:if secret{InputType::Password}else{InputType::Text},class:Some("w-full max-w-xs".into()),value:Some(value),placeholder,disabled,aria_label:Some(label.clone()),onkeydown:move|event:KeyboardEvent|if event.key()==Key::Named(NamedKey::Escape){event.prevent_default();event.stop_propagation();escape_send.call(json!({"method":"input","params":{"data":"\x1b"}}));},onchange:move|input: FormEvent|send.call(super::event("change",&sf,&sid,json!({"item":item,"value":input.value()})))}}}},
    "multi"=>multi_control(control,&prefs_id,&surface,&row_id,disabled,send),
            "action"=>{let act=control.get("act").and_then(Value::as_str).unwrap_or("activate").to_owned();let text=control.get("label").and_then(Value::as_str).unwrap_or("Open").to_owned();let event_id=prefs_id.clone();let sf=surface.clone();rsx!{Button{size:ButtonSize::Sm,variant:ButtonVariant::Outline,disabled,onclick:move |_|send.call(event("action",&sf,&event_id,json!({"act":act,"value":row_id}))),"{text}"}}},
            _=>rsx!{span{class:"text-sm text-muted-foreground","Unsupported setting control: {kind}"}}
        }}
    }}
}
fn key_chord(value: &Value) -> String {
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("+")
        })
        .unwrap_or_else(|| text_value(value))
}
