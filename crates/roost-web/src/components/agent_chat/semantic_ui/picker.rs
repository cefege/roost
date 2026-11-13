//! Native data-first picker with program-owned search, order and selection.
//! Rows compose the controlled `Command` primitives with built-in filtering and
//! key handling disabled, so selection stays the authoritative OMP value.
//! Scoped row ids, ARIA selection and contained reveal keep keyboard navigation
//! visible while focus stays in the native query field.

use super::{event, prop_text, render_node, text_value};
use crate::components::ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Card, CardSize, Command,
    CommandGroupLabel, CommandList, Kbd, StatusDot, Tabs, TabsList, TabsTrigger,
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::{Value, json};
use std::collections::HashMap;

mod item;
mod reveal;
use item::{dot_status, picker_disabled, picker_item};
use reveal::{PickerReveal, picker_item_element_id, picker_list_element_id};

pub(super) fn render_picker(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &std::collections::BTreeMap<String, OmpUiBlob>,
) -> Element {
    let send = *send;
    let id = node.id.clone();
    let sf = surface.to_owned();
    let props = &node.p;
    let title = prop_text(props, "title");
    let subtitle = props.get("subtitle").map(text_value);
    let items = props.get("items").and_then(Value::as_array);
    let query = props.get("query").and_then(Value::as_str);
    let placeholder = props
        .get("placeholder")
        .and_then(Value::as_str)
        .unwrap_or("Search")
        .to_owned();
    // The program owns the query text only when the wire sends a string; an
    // absent or null `query` means printable keys stay the program's commands.
    let show_query = matches!(props.get("query"), Some(Value::String(_)));
    let state = props
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("ready");
    let message = props.get("message").map(text_value);
    let loading_message = message.clone().unwrap_or_else(|| "Loading…".into());
    let error_message = message.unwrap_or_else(|| "Unable to load options.".into());
    let empty_label = props
        .get("empty")
        .map(text_value)
        .unwrap_or_else(|| "No matching options.".into());
    let selected = props.get("selected").and_then(Value::as_str);
    let total = props.get("total").and_then(Value::as_u64);
    let current = props.get("current").and_then(Value::as_array);
    let in_use = current.map(|values| values.iter().map(text_value).collect::<Vec<_>>().join(", "));
    let scope = props.get("scope").and_then(Value::as_str);
    let tab = props.get("tab").and_then(Value::as_str);
    let layout = props
        .get("layout")
        .and_then(Value::as_str)
        .unwrap_or("rows");
    let columns = props
        .get("columns")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let global_hits = props.get("hits");
    let order = props.get("order").and_then(Value::as_array);
    let indexed_items = order.map(|_| {
        items
            .into_iter()
            .flatten()
            .filter_map(|item| {
                item.get("id")
                    .and_then(Value::as_str)
                    .map(|item_id| (item_id, item))
            })
            .collect::<HashMap<_, _>>()
    });
    let shown_ids = match order {
        Some(order) => order
            .iter()
            .filter_map(Value::as_str)
            .filter(|entry| {
                indexed_items
                    .as_ref()
                    .is_some_and(|items| items.contains_key(*entry))
            })
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        None => items
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    };
    let item_count = shown_ids.len();
    let selected_dom = selected
        .filter(|value| shown_ids.iter().any(|shown| shown.as_str() == *value))
        .map(|value| picker_item_element_id(&sf, &id, value));
    let list_active = props
        .get("focus")
        .and_then(Value::as_str)
        .is_none_or(|focus| focus == "list");
    let list_id = picker_list_element_id(&sf, &id);
    let list_label = title.clone().unwrap_or_else(|| placeholder.clone());
    let list_class = if layout == "cards" {
        "grid min-h-0 max-h-[min(60dvh,36rem)] grid-cols-1 gap-2 overscroll-contain sm:grid-cols-2"
    } else {
        "flex min-h-0 max-h-[min(60dvh,36rem)] flex-col gap-1 overscroll-contain"
    };
    let show_preview = props.get("preview").and_then(Value::as_str) != Some("none");
    let focus_id = id.clone();
    let focus_sf = sf.clone();
    let focus_send = send;
    rsx! { div { class: "flex min-w-0 flex-col gap-3", onfocusin:move |_|focus_send.call(event("focus",&focus_sf,&focus_id,json!({}))),
        if let Some(title)=title { h2 { class: "text-lg font-semibold", "{title}" } }
        if let Some(subtitle)=subtitle { p { class: "text-sm text-muted-foreground", "{subtitle}" } }
        if show_query { super::query::NativeQuery { key:"{sf}/{id}", id:id.clone(), surface:sf.clone(), text:query.unwrap_or_default().to_owned(), cursor:props.get("cursor").and_then(Value::as_u64).and_then(|cursor|usize::try_from(cursor).ok()), edit_ack:props.get("queryAck").cloned(), placeholder:placeholder.clone(), send } }
        if let Some(scopes)=props.get("scopes").and_then(Value::as_array) { div { class: "flex min-w-0 gap-2 overflow-x-auto", for value in scopes {
            {
                let choice=value.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                let label=value.get("label").map(text_value).unwrap_or_else(||choice.clone());
                let disabled=picker_disabled(value.get("disabled"));
                let hint=value.get("disabled").and_then(Value::as_str).map(str::to_owned);
                let count=value.get("count").and_then(Value::as_u64).map(|count|count.to_string());
                let dot=value.get("dot").and_then(Value::as_str).map(dot_status);
                let active=scope==Some(choice.as_str());
                let sid=id.clone();let ssf=sf.clone();
                rsx! { Button {
                    size:ButtonSize::Sm,
                    variant:if active{ButtonVariant::Secondary}else{ButtonVariant::Outline},
                    class:"shrink-0",
                    disabled,title:hint,
                    onclick:move |_|send.call(event("action",&ssf,&sid,json!({"act":"scope","value":choice}))),
                    span { class:"inline-flex items-center gap-2",
                        if let Some(status)=dot { StatusDot { status, title:label.clone() } }
                        span { "{label}" }
                        if let Some(count)=count { Badge { variant:BadgeVariant::Muted,class:Some("text-xs".into()),"{count}" } }
                    }
                }}
            }
        } } }
        if let Some(tabs)=props.get("tabs").and_then(Value::as_array) {
            {
                let active=tab.unwrap_or_default().to_owned();
                let sid=id.clone();let ssf=sf.clone();
                rsx! { Tabs { value:active, handles_keys:false,
                    on_value_change:move |choice:String|send.call(event("action",&ssf,&sid,json!({"act":"tab","value":choice}))),
                    TabsList { class:Some("h-auto max-w-full flex-wrap".into()), aria_label:Some("Picker views".into()),
                        for value in tabs {
                            {let choice=value.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();let label=value.get("label").map(text_value).unwrap_or_else(||choice.clone());rsx!{TabsTrigger{key:"{choice}",value:choice,has_panel:false,"{label}"}}}
                        }
                    }
                }}
            }
        }
        if let Some(strip)=props.get("strip").filter(|value|!value.is_null()) { Card { size:CardSize::Sm, class:Some("flex-row flex-wrap items-center gap-2 px-2 py-2".into()), if let Some(label)=strip.get("label").map(text_value) { span { class: "text-xs text-muted-foreground", "{label}" } }
            for chip in strip.get("items").and_then(Value::as_array).into_iter().flatten() { {let choice=chip.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();let label=chip.get("label").map(text_value).unwrap_or_else(||choice.clone());let active=chip.get("on").and_then(Value::as_bool).unwrap_or(false)||strip.get("selected").and_then(Value::as_str)==Some(choice.as_str());let sid=id.clone();let ssf=sf.clone();rsx!{Button{size:ButtonSize::Sm,variant:if active{ButtonVariant::Secondary}else{ButtonVariant::Outline},onclick:move |_|send.call(event("action",&ssf,&sid,json!({"act":"strip","value":choice}))),"{label}"}}} }
        } }
        if state=="loading" { p { role:"status", class:"text-sm text-muted-foreground", "{loading_message}" } }
        else if state=="error" { p { role:"alert", class:"text-sm text-destructive", "{error_message}" } }
        else {
            if let Some(total)=total { p { class:"text-xs text-muted-foreground", "{item_count} of {total}" } }
            if let Some(in_use)=in_use { p { class:"text-xs text-muted-foreground", "In use: {in_use}" } }
            Command { class:"h-auto w-full overflow-visible", should_filter:false, handles_keys:false,
                CommandList { id:list_id.clone(), role:"listbox", aria_label:Some(list_label), tabindex:Some("-1".into()), class:Some(list_class.into()),
                    aria_active_descendant:if list_active{selected_dom.clone()}else{None},
                    if let Some(order)=order { for entry in order {
                        if let Some(group)=entry.get("group").and_then(Value::as_str) { {let group_label=entry.get("label").map(text_value).unwrap_or_else(||group.to_owned());rsx!{CommandGroupLabel{class:Some("col-span-full border-b border-border py-1".into()),aria_hidden:Some("true".into()),"{group_label}"}}} }
                        else if let Some(item_id)=entry.as_str() { if let Some(item)=indexed_items.as_ref().and_then(|items|items.get(item_id)).copied() { {picker_item(item,selected,current.map(Vec::as_slice),&id,&sf,layout,columns,global_hits,&send)} } }
                    } }
                    else { for item in items.into_iter().flatten() { {picker_item(item,selected,current.map(Vec::as_slice),&id,&sf,layout,columns,global_hits,&send)} } }
                    if items.is_some_and(|rows|rows.is_empty()) || (order.is_some_and(|rows|rows.is_empty())) { p { class:"p-3 text-sm text-muted-foreground", "{empty_label}" } }
                }
            }
            if let Some(item_id)=selected_dom { PickerReveal { selected:selected.map(str::to_owned), list_id:list_id.clone(), item_id } }
        }
        if let Some(confirm)=props.get("confirm").filter(|value|!value.is_null()) {
            {
                let act=confirm.get("act").and_then(Value::as_str).unwrap_or("confirm").to_owned();
                let label=confirm.get("label").map(text_value).filter(|label|!label.is_empty()).unwrap_or_else(||"Confirm".into());
                let text=confirm.get("text").map(text_value).filter(|text|!text.is_empty());
                let disabled=selected.is_none();
                let sid=id.clone();
                let ssf=sf.clone();
                rsx!{
                    div { class:"flex flex-wrap items-center gap-2", "data-omp-picker-confirm":"",
                        if let Some(text)=text { p { class:"text-sm", "{text}" } }
                        Button{size:ButtonSize::Sm,variant:ButtonVariant::Default,disabled,onclick:move |_|send.call(event("action",&ssf,&sid,json!({"act":act}))),"{label}"}
                    }
                }
            }
        }
        if let Some(actions)=props.get("actions").and_then(Value::as_array) {
            div { class:"flex flex-wrap gap-2 border-t border-border pt-2",
                for action in actions {
                    {
                        let act=action.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                        let label=action.get("label").and_then(Value::as_str).unwrap_or(&act).to_owned();
                        let disabled=picker_disabled(action.get("disabled"));
                        let active=action.get("on").and_then(Value::as_bool).unwrap_or(false);
                        let danger=action.get("danger").and_then(Value::as_bool)==Some(true);
                        let keys=action.get("keys").and_then(Value::as_array).map(|values|values.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" + "));
                        let sid=id.clone();let ssf=sf.clone();
                        rsx! { Button {
                            size:ButtonSize::Sm,
                            variant:if danger{ButtonVariant::Destructive}else if action.get("primary").and_then(Value::as_bool)==Some(true){ButtonVariant::Default}else if active{ButtonVariant::Secondary}else{ButtonVariant::Outline},
                            disabled,title:action.get("disabled").and_then(Value::as_str).map(str::to_owned),
                            onclick:move |_|send.call(event("action",&ssf,&sid,json!({"act":act}))),
                            "{label}"
                            if let Some(keys)=keys { Kbd { "{keys}" } }
                        }}
                    }
                }
            }
        }
        if show_preview { for child in &node.c { {render_node(child,surface,&send,blobs)} } }
    } }
}
