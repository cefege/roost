//! One native picker row and its fact, hit and badge presentation.
//! Rows compose the controlled `CommandItem` so the authoritative OMP selection
//! owns ARIA state while pointer events still send `select`/`activate`.

use super::super::{event, text_value};
use super::reveal::picker_item_element_id;
use crate::components::ui::{Badge, BadgeVariant, CommandItem, StatusDot};
use dioxus::prelude::*;
use serde_json::{Value, json};

const PICKER_ROW_CLASS: &str = "h-auto w-full justify-start whitespace-normal text-left [contain-intrinsic-size:auto_2.5rem] [content-visibility:auto]";

#[allow(clippy::too_many_arguments)]
pub(super) fn picker_item(
    item: &Value,
    selected: Option<&str>,
    current: Option<&[Value]>,
    id: &str,
    surface: &str,
    layout: &str,
    columns: Option<&[Value]>,
    global_hits: Option<&Value>,
    send: &EventHandler<Value>,
) -> Element {
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let label = item
        .get("label")
        .map(text_value)
        .unwrap_or_else(|| item_id.clone());
    let detail = item.get("detail").map(text_value);
    let disabled = picker_disabled(item.get("disabled"));
    let is_selected = selected == Some(item_id.as_str());
    let is_current =
        current.is_some_and(|values| values.iter().any(|value| text_value(value) == item_id));
    let title = item.get("title").and_then(Value::as_str).map(str::to_owned);
    let button_title = item
        .get("disabled")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(title);
    let mark = item
        .get("mark")
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let icon = item.get("icon").and_then(Value::as_str).map(str::to_owned);
    let dot = item
        .get("dot")
        .or_else(|| item.get("tone"))
        .and_then(Value::as_str)
        .map(dot_status);
    let role = item.get("role").and_then(Value::as_str).map(str::to_owned);
    let depth = item.get("depth").and_then(Value::as_u64).unwrap_or(0);
    let indent = match depth {
        0 => "pl-0",
        1 => "pl-2",
        2 => "pl-4",
        _ => "pl-6",
    };
    let node_kind = item.get("node").and_then(Value::as_str).map(str::to_owned);
    let is_open = item.get("open").and_then(Value::as_bool);
    let mono = item.get("mono").and_then(Value::as_bool) == Some(true);
    let item_hits = item
        .get("hits")
        .or_else(|| global_hits.and_then(|hits| hits.get(item_id.as_str())));
    let element_id = picker_item_element_id(surface, id, &item_id);
    let send = *send;
    let event_id = id.to_owned();
    let sf = surface.to_owned();
    let choice = item_id.clone();
    let on_select = (!disabled).then(|| {
        EventHandler::new(move |_: ()| {
            let verb = if is_selected { "activate" } else { "select" };
            send.call(event(verb, &sf, &event_id, json!({"item": choice})));
        })
    });
    let key_event_id = id.to_owned();
    let key_sf = surface.to_owned();
    let key_choice = item_id.clone();
    rsx! {CommandItem{key:"{item_id}",id:element_id,value:item_id.clone(),focusable:false,show_check:false,selected:is_selected,
        class:Some(PICKER_ROW_CLASS.into()),
        "data-omp-picker-item":item_id.clone(),
        "data-current":is_current.then_some("true"),
        "data-disabled":disabled.then_some("true"),
        "aria-disabled":disabled.then_some("true"),
        "aria-current":is_current.then_some("true"),
        title:button_title,
        on_select:on_select,
        onkeydown:move |key_event: KeyboardEvent| {
            if disabled || key_event.is_composing() {
                return;
            }
            let key = key_event.key();
            if key != Key::Named(NamedKey::Enter) && key != Key::Character(" ".into()) {
                return;
            }
            key_event.prevent_default();
            key_event.stop_propagation();
            let verb = if is_selected { "activate" } else { "select" };
            send.call(event(verb, &key_sf, &key_event_id, json!({"item": key_choice})));
        },
        div{class:if layout=="cards"{"flex min-w-0 flex-col items-start gap-1"}else{"flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1"},span{class:"flex min-w-0 items-center gap-2 {indent}",if let Some(status)=dot{StatusDot{status}}if let Some(mark)=mark{Badge{variant:BadgeVariant::Secondary,class:Some("shrink-0".into()),"{mark}"}}if let Some(icon)=icon{Badge{variant:BadgeVariant::Muted,class:Some("shrink-0 text-xs".into()),"{icon}"}}if let Some(role)=role{Badge{variant:BadgeVariant::Muted,class:Some("text-xs".into()),"{role}"}}if let Some(node_kind)=node_kind{Badge{variant:BadgeVariant::Outline,class:Some("text-xs".into()),"{node_kind}"}}{render_picker_label(&label,item_hits,mono)}}
            if let Some(detail)=detail {span{class:"text-xs text-muted-foreground","{detail}"}}
            if let Some(facts)=item.get("facts").and_then(Value::as_object) {{render_picker_facts(facts,columns)}}
            if let Some(open_label)=is_open.map(|is_open|if is_open{"Open"}else{"Closed"}) {Badge{variant:if open_label=="Open"{BadgeVariant::Secondary}else{BadgeVariant::Muted},class:Some("text-xs".into()),"{open_label}"}}
            if let Some(badges)=item.get("badges").and_then(Value::as_array) {div{class:"flex flex-wrap gap-1",for badge in badges { {let badge_text=badge.get("text").and_then(Value::as_str).unwrap_or_default();let variant=badge.get("tone").and_then(Value::as_str).map(badge_variant).unwrap_or(BadgeVariant::Outline);rsx!{Badge{variant,class:Some("text-xs".into()),"{badge_text}"}}} }}}
            if let Some(chips)=item.get("chips").and_then(Value::as_array) {div{class:"flex flex-wrap gap-1",for chip in chips { {let chip_text=chip.get("text").and_then(Value::as_str).unwrap_or_default();let variant=if chip.get("on").and_then(Value::as_bool)==Some(true){BadgeVariant::Secondary}else{BadgeVariant::Muted};rsx!{Badge{variant,class:Some("text-xs".into()),"{chip_text}"}}} }}}
        }
    }}
}

fn render_picker_facts(
    facts: &serde_json::Map<String, Value>,
    columns: Option<&[Value]>,
) -> Element {
    if let Some(columns) = columns {
        rsx! {div{class:"flex flex-wrap gap-x-3 text-xs text-muted-foreground",for column in columns{
            if let Some(key)=column.get("id").and_then(Value::as_str){
                if let Some(value)=facts.get(key){
                    {let label=column.get("head").and_then(Value::as_str).unwrap_or(key);let text=text_value(value);let title=column.get("head").and_then(Value::as_str);rsx!{span{title,"{label}: {text}"}}}
                }
            }
        }}}
    } else {
        rsx! {div{class:"flex flex-wrap gap-x-3 text-xs text-muted-foreground",for(key,value)in facts{
            {let text=text_value(value);rsx!{span{"{key}: {text}"}}}
        }}}
    }
}

fn render_picker_label(label: &str, hits: Option<&Value>, mono: bool) -> Element {
    let class = if mono {
        "break-all font-mono"
    } else {
        "break-words"
    };
    let mut ranges = hits
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|range| {
            let values = range.as_array()?;
            let start = usize::try_from(values.first()?.as_u64()?).ok()?;
            let end = usize::try_from(values.get(1)?.as_u64()?).ok()?;
            let start = utf16_byte_offset(label, start)?;
            let end = utf16_byte_offset(label, end)?;
            (start < end).then_some((start, end))
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|range| range.0);
    let mut segments = Vec::new();
    let mut cursor = 0;
    for (start, end) in ranges {
        if start < cursor {
            continue;
        }
        if start > cursor {
            segments.push((&label[cursor..start], false));
        }
        segments.push((&label[start..end], true));
        cursor = end;
    }
    if cursor < label.len() {
        segments.push((&label[cursor..], false));
    }
    rsx! {span{class,for(text,matched)in segments{if matched{span{class:"rounded-sm bg-primary/15 text-primary","{text}"}}else{span{"{text}"}}}}}
}

fn utf16_byte_offset(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units = units.checked_add(character.len_utf16())?;
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

pub(super) fn picker_disabled(value: Option<&Value>) -> bool {
    value.is_some_and(|value| !value.is_null() && value != false)
}

fn badge_variant(tone: &str) -> BadgeVariant {
    match tone {
        "success" => BadgeVariant::Success,
        "warning" => BadgeVariant::Warning,
        "error" | "danger" => BadgeVariant::Destructive,
        "info" => BadgeVariant::Info,
        "muted" => BadgeVariant::Muted,
        _ => BadgeVariant::Outline,
    }
}

pub(super) fn dot_status(tone: &str) -> String {
    match tone {
        "success" | "ok" | "done" => "ok",
        "running" | "accent" => "running",
        "warning" | "warn" => "warn",
        "error" | "danger" => "error",
        "info" => "info",
        _ => "idle",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::utf16_byte_offset;

    #[test]
    fn picker_hits_resolve_utf16_boundaries_without_splitting_surrogates() {
        assert_eq!(utf16_byte_offset("a😀b", 1), Some(1));
        assert_eq!(utf16_byte_offset("a😀b", 2), None);
        assert_eq!(utf16_byte_offset("a😀b", 3), Some(5));
        assert_eq!(utf16_byte_offset("a😀b", 4), Some(6));
    }
}
