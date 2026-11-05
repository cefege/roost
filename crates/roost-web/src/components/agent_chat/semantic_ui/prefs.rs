//! Native settings pages, search and terminal-compatible key capture.
//! Row controls preserve the prefs owner and individual setting addresses.
//! Changes are interpreted and persisted by OMP's settings controller.
mod multi;
mod row;
use super::{event, text_value};
use crate::components::ui::{Button, ButtonSize, ButtonVariant};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use row::pref_row;
use serde_json::{Value, json};

pub(super) fn render_prefs(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &std::collections::BTreeMap<String, OmpUiBlob>,
) -> Element {
    let send = *send;
    let id = node.id.clone();
    let sf = surface.to_owned();
    let props = &node.p;
    let title = props
        .get("title")
        .map(text_value)
        .unwrap_or_else(|| "Settings".into());
    let page = props
        .get("page")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let query = props.get("query").and_then(Value::as_str);
    let editing = props.get("editing").filter(|value| !value.is_null());
    let focused = props.get("focus").and_then(Value::as_str);
    let capture_keys = editing
        .and_then(|value| value.get("row"))
        .and_then(Value::as_str)
        .is_some_and(|row_id| {
            props
                .get("sections")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|section| section.get("rows").and_then(Value::as_array))
                .flatten()
                .any(|row| {
                    row.get("id").and_then(Value::as_str) == Some(row_id)
                        && row
                            .get("control")
                            .and_then(|control| control.get("k"))
                            .and_then(Value::as_str)
                            == Some("keys")
                })
        });
    rsx! { div { class:"flex min-w-0 flex-col gap-4",
        onkeydown:move|event:KeyboardEvent|{if capture_keys&& let Some(data)=super::keyboard::input_data(&event){event.prevent_default();event.stop_propagation();send.call(json!({"method":"input","params":{"data":data}}));}},
        h2 { class:"text-lg font-semibold", "{title}" }
        if let Some(lead)=props.get("lead").and_then(Value::as_str) { p { class:"text-sm text-muted-foreground", "{lead}" } }
        super::query::NativeQuery { key:"{sf}/{id}", id:id.clone(), surface:sf.clone(), text:query.unwrap_or_default().to_owned(), cursor:props.get("cursor").and_then(Value::as_u64).and_then(|cursor|usize::try_from(cursor).ok()), edit_ack:props.get("queryAck").cloned(), focused:editing.is_none(), placeholder:"Search settings", send }
        if let Some(pages)=props.get("pages").and_then(Value::as_array) { div { class:"flex flex-wrap gap-2", for tab in pages {
            {let target=tab.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();let label=tab.get("label").map(text_value).unwrap_or_else(||target.clone());let active=page==target;let disabled=tab.get("disabled").is_some_and(|v|!v.is_null()&&v!=false);let sid=id.clone();let ssf=sf.clone();rsx!{Button{size:ButtonSize::Sm,variant:if active{ButtonVariant::Secondary}else{ButtonVariant::Ghost},disabled,onclick:move |_|send.call(event("action",&ssf,&sid,json!({"act":"page","value":target}))),"{label}"}}}
        } } }
        if let Some(sections)=props.get("sections").and_then(Value::as_array) { for group in sections {
            {let group_title=group.get("title").map(text_value).unwrap_or_else(||"Settings".into());let rows=group.get("rows").and_then(Value::as_array);rsx!{section{class:"flex flex-col gap-2",h3{class:"font-semibold","{group_title}"} for row in rows.into_iter().flatten(){ {pref_row(row,&id,&sf,editing,focused,&send)} }}}}
        } }
        for child in &node.c { {super::render_node(child,surface,&send,blobs)} }
    } }
}
