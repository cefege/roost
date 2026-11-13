//! Native settings pages, search and terminal-compatible key capture.
//! Row controls preserve the prefs owner and individual setting addresses.
//! Changes are interpreted and persisted by OMP's settings controller.
mod choice;
mod multi;
mod reveal;
mod row;
mod text;
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
    let active_page = search_focus_page(props.get("sections"), query, focused).unwrap_or(page);
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
    let no_results = query.is_some_and(|text| !text.is_empty())
        && !props
            .get("sections")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|section| {
                section
                    .get("rows")
                    .and_then(Value::as_array)
                    .is_some_and(|rows| !rows.is_empty())
            });
    rsx! { div { class:"flex min-w-0 flex-col gap-4",
        "data-omp-page": page,
        onkeydown:move|event:KeyboardEvent|{if capture_keys&& let Some(data)=super::keyboard::input_data(&event){event.prevent_default();event.stop_propagation();send.call(json!({"method":"input","params":{"data":data}}));}},
        h2 { class:"text-lg font-semibold", "{title}" }
        div { inert: editing.is_some().then_some(true),
            super::query::NativeQuery { key:"{sf}/{id}", id:id.clone(), surface:sf.clone(), text:query.unwrap_or_default().to_owned(), cursor:props.get("cursor").and_then(Value::as_u64).and_then(|cursor|usize::try_from(cursor).ok()), edit_ack:props.get("queryAck").cloned(), focused:editing.is_none(), placeholder:"Search settings", send }
        }
        if let Some(pages)=props.get("pages").and_then(Value::as_array) { div { class:"flex flex-wrap gap-2", for tab in pages {
            {
                let target=tab.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                let label=tab.get("label").map(text_value).unwrap_or_else(||target.clone());
                let active=active_page==target;
                let disabled=tab.get("disabled").is_some_and(|value|!value.is_null()&&value!=false);
                let reason=tab.get("disabled").and_then(Value::as_str);
                let icon=tab.get("icon").and_then(Value::as_str).map(|name|super::content::render_icon(name,"size-4"));
                let group=tab.get("group").and_then(Value::as_str);
                let sid=id.clone();
                let ssf=sf.clone();
                let tab_attribute=target.clone();
                rsx!{
                    div{class:"inline-flex items-center gap-2",title:reason,
                        "data-omp-page-tab": tab_attribute,
                        "data-omp-page-active": active.to_string(),
                        if let Some(group)=group {span{class:"text-xs text-muted-foreground","{group}"}}
                        Button{size:ButtonSize::Sm,variant:if active{ButtonVariant::Secondary}else{ButtonVariant::Ghost},disabled,onclick:move |_|if !disabled{send.call(event("action",&ssf,&sid,json!({"act":"page","value":target})));},{icon} "{label}"}
                    }
                }
            }
        } } }
        if let Some(sections)=props.get("sections").and_then(Value::as_array) { for (group,heading) in with_page_headings(sections,props.get("pages").and_then(Value::as_array).map(Vec::as_slice)) {
            {let group_title=group.get("title").map(text_value).unwrap_or_else(||"Settings".into());let rows=group.get("rows").and_then(Value::as_array);rsx!{
                if let Some((page_id,page_label,page_icon))=heading {h3{class:"flex items-center gap-2 font-semibold","data-omp-section-page":page_id,{page_icon}"{page_label}"}}
                section{class:"flex flex-col gap-2",if !group_title.is_empty(){h3{class:"font-semibold","{group_title}"}} for row in rows.into_iter().flatten(){ {pref_row(row,&id,&sf,editing,focused,&send)} }}}}
        } }
        if no_results { p { class:"text-sm text-muted-foreground", "data-omp-page-empty":"true", "No matching settings" } }
        // An open option picker is a child of the prefs node, outside any row; it carries the row it edits so its items stay attributable to that setting.
        if !node.c.is_empty() { div { class: "contents", "data-omp-setting": focused.map(str::to_owned), for child in &node.c { {super::render_node(child,surface,&send,blobs)} } } }
    } }
}

fn search_focus_page<'a>(
    sections: Option<&'a Value>,
    query: Option<&str>,
    focused: Option<&str>,
) -> Option<&'a str> {
    let focused = focused.filter(|_| query.is_some_and(|text| !text.is_empty()))?;
    sections?
        .as_array()?
        .iter()
        .find(|section| {
            section
                .get("rows")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row.get("id").and_then(Value::as_str) == Some(focused))
                })
        })?
        .get("page")?
        .as_str()
}

type PageHeading = (String, String, Option<Element>);

/// Search results come grouped by page; the first section of each page run carries that page's
/// heading (its nav label without the hit count), which is how the terminal groups the same rows.
fn with_page_headings<'a>(
    sections: &'a [Value],
    pages: Option<&[Value]>,
) -> Vec<(&'a Value, Option<PageHeading>)> {
    let mut previous: Option<&str> = None;
    sections
        .iter()
        .map(|section| {
            let current = section.get("page").and_then(Value::as_str);
            let heading = current.filter(|id| previous != Some(*id)).map(|id| {
                let tab = pages.and_then(|pages| {
                    pages
                        .iter()
                        .find(|tab| tab.get("id").and_then(Value::as_str) == Some(id))
                });
                let label = tab
                    .and_then(|tab| tab.get("label"))
                    .map(text_value)
                    .unwrap_or_else(|| id.to_owned());
                let icon = tab
                    .and_then(|tab| tab.get("icon"))
                    .and_then(Value::as_str)
                    .map(|name| super::content::render_icon(name, "size-4"));
                (id.to_owned(), strip_hit_count(&label), icon)
            });
            previous = current;
            (section, heading)
        })
        .collect()
}

fn strip_hit_count(label: &str) -> String {
    match label.rsplit_once(" (") {
        Some((name, count))
            if count.strip_suffix(')').is_some_and(|digits| {
                !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_digit())
            }) =>
        {
            name.to_owned()
        }
        _ => label.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::search_focus_page;
    use serde_json::json;

    #[test]
    fn search_tab_follows_the_focused_result() {
        let props = json!({"sections": [
            {"page": "model", "rows": [{"id": "externalThinking"}]},
            {"page": "context", "rows": [{"id": "compaction.methodOrder"}]}
        ]});
        assert_eq!(
            search_focus_page(
                props.get("sections"),
                Some("thinking"),
                Some("externalThinking")
            ),
            Some("model")
        );
        assert_eq!(
            search_focus_page(
                props.get("sections"),
                Some("order"),
                Some("compaction.methodOrder")
            ),
            Some("context")
        );
        assert_eq!(
            search_focus_page(props.get("sections"), Some("order"), None),
            None
        );
        assert_eq!(
            search_focus_page(props.get("sections"), Some(""), Some("externalThinking")),
            None
        );
    }
}
