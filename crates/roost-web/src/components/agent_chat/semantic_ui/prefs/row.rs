//! Native preference rows and their owning prefs-node addresses.
//! The page renderer borrows each row; only event routing fields are retained.
//! Control changes are handled by OMP rather than local settings state.
use super::super::{event, text_value};
use super::choice::choice_control;
use super::multi::multi_control;
use super::reveal::PrefsRevealFocus;
use super::text::PrefText;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Input, InputType, Switch};
use dioxus::prelude::*;
use serde_json::{Value, json};

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
    let kind = control
        .get("k")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    let disabled = row
        .get("disabled")
        .is_some_and(|value| !value.is_null() && value != false);
    let disabled_reason = row
        .get("disabled")
        .and_then(Value::as_str)
        .map(str::to_owned);
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
    let focus_row = row_id.clone();
    let can_focus_row = editing.is_none() && !focused_now && !disabled;
    let focus_send = send;
    rsx! {div{class:match (focused_now, kind) {
        (true, "switch")=>"flex items-center justify-between gap-2 border-b border-ring py-2",
        (false, "switch")=>"flex items-center justify-between gap-2 border-b border-border py-2",
        (true, "multi")=>"flex flex-col items-stretch justify-between gap-2 border-b border-ring py-2 sm:flex-row sm:items-start",
        (false, "multi")=>"flex flex-col items-stretch justify-between gap-2 border-b border-border py-2 sm:flex-row sm:items-start",
        (true, _)=>"flex flex-col items-stretch justify-between gap-2 border-b border-ring py-2 sm:flex-row sm:items-center",
        (false, _)=>"flex flex-col items-stretch justify-between gap-2 border-b border-border py-2 sm:flex-row sm:items-center",
    },title:disabled_reason,onfocusin:move |_|if can_focus_row {focus_send.call(event("select",&focus_surface,&focus_id,json!({"item":focus_row})));},
        "data-omp-setting": row_id.clone(),
        "data-omp-control": kind,
        if focused_now { PrefsRevealFocus { row_id: row_id.clone() } }
        div{class:"min-w-0 flex-1",p{class:"text-sm font-medium", "{label}" if changed {span{class:"ml-2 text-primary",title:default_label,"•"}}} if let Some(warning)=warning{p{class:"text-xs text-warning","{warning}"}} if let Some(hint)=hint{p{class:"text-xs text-muted-foreground","{hint}"}}}
        {match kind{
            "switch"=>{let value=control.get("on").and_then(Value::as_bool).unwrap_or(false);let event_id=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();rsx!{Switch{checked:value,disabled,aria_label:label.clone(),on_checked_change:move|value|send.call(event("change",&sf,&event_id,json!({"item":item,"value":value})))}}},
            "choice"=>rsx!{div{class:if control.get("mono").and_then(Value::as_bool).unwrap_or(false){"font-mono"}else{""},{choice_control(control,&prefs_id,&surface,&row_id,&label,disabled,editing,send)}}},
            "number"=>{let value=control.get("value").map(text_value).unwrap_or_default();let min=control.get("min").map(text_value);let max=control.get("max").map(text_value);let step=control.get("step").map(text_value);let unit=control.get("unit").and_then(Value::as_str).map(str::to_owned);let step_label=control.get("labels").and_then(Value::as_object).and_then(|labels|labels.get(&value)).and_then(Value::as_str).map(str::to_owned);let sid=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();rsx!{div{class:"flex flex-wrap items-center gap-2",Input{r#type:InputType::Number,class:Some("w-32".into()),value:Some(value),min,max,step,disabled,aria_label:Some(label.clone()),onchange:move|input: FormEvent|{if let Ok(number)=input.value().parse::<f64>()&& number.is_finite(){send.call(super::event("change",&sf,&sid,json!({"item":item,"value":number})));}}}if let Some(step_label)=step_label{span{class:"text-sm text-muted-foreground","data-omp-step-label":"true","{step_label}"}}if let Some(unit)=unit{span{class:"text-sm text-muted-foreground","{unit}"}}}}},
            "keys"=>{let keys=control.get("keys").and_then(Value::as_array);let active=editing.and_then(|value|value.get("row")).and_then(Value::as_str)==Some(row_id.as_str());let display=keys.map(|keys|keys.iter().map(key_chord).collect::<Vec<_>>().join(", ")).unwrap_or_default();let button_label=if active{"Press a key…"}else if display.is_empty(){"Set shortcut"}else{display.as_str()};let sid=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();rsx!{Button{size:ButtonSize::Sm,variant:if active{ButtonVariant::Secondary}else{ButtonVariant::Outline},disabled,onclick:move |_|send.call(event("action",&sf,&sid,json!({"act":"edit","value":item}))),"{button_label}"}}},
            "text"=>{
                let secret=control.get("secret").and_then(Value::as_bool).unwrap_or(false);
                let mono=control.get("mono").and_then(Value::as_bool).unwrap_or(false);
                let active=editing.filter(|value|value.get("row").and_then(Value::as_str)==Some(row_id.as_str()));
                if let Some(active)=active {
                    let secret=secret||active.get("secret").and_then(Value::as_bool).unwrap_or(false);
                    let draft=active.get("draft").and_then(Value::as_str).unwrap_or_default().to_owned();
                    let cursor=active.get("cursor").and_then(Value::as_u64).and_then(|cursor|usize::try_from(cursor).ok());
                    let ack=active.get("ack").cloned();
                    let placeholder=control.get("placeholder").and_then(Value::as_str).map(str::to_owned);
                    rsx!{PrefText{key:"{row_id}",id:prefs_id.clone(),surface:surface.clone(),text:draft,cursor,ack,placeholder,label:label.clone(),disabled,secret,mono,send}}
                }else{
                    let shown=control.get("value").and_then(Value::as_str).filter(|value|!value.is_empty()).or_else(||control.get("placeholder").and_then(Value::as_str)).unwrap_or("Edit…").to_owned();
                    let sid=prefs_id.clone();
                    let sf=surface.clone();
                    let item=row_id.clone();
                    rsx!{Button{size:ButtonSize::Sm,variant:ButtonVariant::Outline,disabled,aria_label:label.clone(),onclick:move |_|send.call(event("action",&sf,&sid,json!({"act":"edit","value":item}))),"{shown}"}}
                }
            },
            "multi"=>multi_control(control,&prefs_id,&surface,&row_id,disabled,send),
            "action"=>{let act=control.get("act").and_then(Value::as_str).unwrap_or("activate").to_owned();let text=control.get("label").and_then(Value::as_str).unwrap_or("Open").to_owned();let event_id=prefs_id.clone();let sf=surface.clone();let item=row_id.clone();rsx!{Button{size:ButtonSize::Sm,variant:ButtonVariant::Outline,disabled,onclick:move |_|send.call(event("action",&sf,&event_id,json!({"act":act,"value":item}))),"{text}"}}},
            _=>rsx!{span{role:"alert","data-omp-render-error":"unsupported-prefs-control",class:"text-sm text-destructive","Unsupported setting control: {kind}"}}
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
