//! Structured key/value, table, tree, and JSON content.
//! Tree and JSON disclosures compose the shared `Collapsible` so the browser's
//! button activation handles Enter and Space instead of forwarding them as
//! terminal input. All collection values stay reachable through the disclosure.

use super::text_value;
use crate::components::ui::{
    ButtonSize, Collapsible, CollapsibleContent, CollapsibleTrigger, Icon, IconType, Table,
    TableBody, TableCell, TableHead, TableHeader, TableRow, TableWrapper,
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

pub(super) fn render(node: &OmpUiNode) -> Element {
    match node.k.as_str() {
        "kv" => key_values(node),
        "table" => table(node),
        "tree" => tree(node),
        "json" => {
            let expanded = node.p.get("open").and_then(Value::as_u64).unwrap_or(0) as usize;
            rsx! {
                JsonTree {
                    value: node.p.get("value").cloned().unwrap_or(Value::Null),
                    depth: 0,
                    expanded,
                }
            }
        }
        _ => rsx! { span {} },
    }
}

fn key_values(node: &OmpUiNode) -> Element {
    let items = node.p.get("items").and_then(Value::as_array);
    let inline = node.p.get("layout").and_then(Value::as_str) == Some("inline");
    let class = if inline {
        "flex flex-wrap gap-x-4 gap-y-1 text-sm"
    } else {
        "flex min-w-0 flex-col gap-1 text-sm"
    };
    rsx! {
        dl { class,
            for item in items.into_iter().flatten() {
                { let key = item.get("k").map(text_value).unwrap_or_default();
                  let value = item.get("v").map(text_value).unwrap_or_default();
                  rsx! {
                      dt { class: "text-muted-foreground", "{key}" }
                      dd { class: "min-w-0 break-all", "{value}" }
                  }
                }
            }
        }
    }
}

fn table(node: &OmpUiNode) -> Element {
    let columns = node.p.get("cols").and_then(Value::as_array);
    let rows = node.p.get("rows").and_then(Value::as_array);
    rsx! {
        TableWrapper { class: Some("w-full".into()),
            Table {
                TableHeader {
                    TableRow {
                        for column in columns.into_iter().flatten() {
                            { let heading = column.get("head").map(text_value).unwrap_or_else(|| {
                                  column.get("id").map(text_value).unwrap_or_default()
                              });
                              rsx! { TableHead { "{heading}" } }
                            }
                        }
                    }
                }
                TableBody {
                    for row in rows.into_iter().flatten() {
                        { let cells = row.get("cells").and_then(Value::as_object);
                          rsx! {
                              TableRow {
                                  for column in columns.into_iter().flatten() {
                                      { let key = column.get("id").and_then(Value::as_str).unwrap_or_default();
                                        let value = cells.and_then(|cells| cells.get(key)).map(text_value).unwrap_or_default();
                                        let class = match column.get("align").and_then(Value::as_str) {
                                            Some("center") => "text-center whitespace-normal",
                                            Some("end") => "text-right whitespace-normal",
                                            _ => "text-left whitespace-normal",
                                        };
                                        rsx! {
                                            TableCell {
                                                class: Some(class.into()),
                                                "{value}"
                                            }
                                        }
                                      }
                                  }
                              }
                          }
                        }
                    }
                }
            }
        }
    }
}

fn tree(node: &OmpUiNode) -> Element {
    let nodes = node.p.get("nodes").and_then(Value::as_array);
    rsx! {
        ul { class: "flex flex-col gap-1",
            for (index, item) in nodes.into_iter().flatten().enumerate() {
                TreeItem { key: "{index}", node: item.clone() }
            }
        }
    }
}

#[component]
fn TreeItem(node: Value) -> Element {
    let label = node.get("label").map(text_value).unwrap_or_default();
    let icon = node
        .get("icon")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let children = node.get("children").and_then(Value::as_array).cloned();
    let initially_open = node.get("open").and_then(Value::as_bool).unwrap_or(false);
    let mut open = use_signal(|| initially_open);
    use_effect(use_reactive((&initially_open,), move |(next,)| {
        open.set(next);
    }));
    if let Some(children) = children.filter(|children| !children.is_empty()) {
        rsx! {
            li { class: "ml-3 flex flex-col gap-1",
                Collapsible { open: open(), on_open_change: move |next: bool| open.set(next),
                    CollapsibleTrigger {
                        size: ButtonSize::Sm,
                        class: Some("h-auto w-full justify-start gap-1 whitespace-normal px-0 py-0 text-left text-sm font-normal".into()),
                        Icon {
                            icon: if open() { IconType::ChevronDown } else { IconType::ChevronRight },
                            class: "size-4 shrink-0 text-muted-foreground",
                        }
                        if !icon.is_empty() { span { class: "mr-1 text-muted-foreground", "{icon}" } }
                        "{label}"
                    }
                    CollapsibleContent { class: Some("pl-3".into()),
                        ul { class: "flex flex-col gap-1",
                            for (index, child) in children.iter().enumerate() {
                                TreeItem { key: "{index}", node: child.clone() }
                            }
                        }
                    }
                }
            }
        }
    } else {
        rsx! {
            li { class: "ml-3 flex flex-col gap-1",
                span { class: "text-sm",
                    if !icon.is_empty() { span { class: "mr-1 text-muted-foreground", "{icon}" } }
                    "{label}"
                }
            }
        }
    }
}

#[component]
fn JsonTree(value: Value, depth: usize, expanded: usize) -> Element {
    let initially_open = depth == 0 || depth <= expanded;
    let mut open = use_signal(|| initially_open);
    use_effect(use_reactive((&initially_open,), move |(next,)| {
        open.set(next);
    }));
    if let Some(entries) = value.as_object() {
        rsx! {
            Collapsible { open: open(), on_open_change: move |next: bool| open.set(next),
                CollapsibleTrigger {
                    size: ButtonSize::Sm,
                    class: Some("h-auto w-full justify-start gap-1 whitespace-normal px-0 py-0 text-left font-mono text-xs font-normal".into()),
                    Icon {
                        icon: if open() { IconType::ChevronDown } else { IconType::ChevronRight },
                        class: "size-3 shrink-0 text-muted-foreground",
                    }
                    "{entries.len()} fields"
                }
                CollapsibleContent { class: Some("pl-3".into()),
                    dl { class: "ml-3 flex flex-col gap-1 text-xs",
                        for (key, child) in entries {
                            dt { class: "text-muted-foreground", "{key}" }
                            dd { JsonTree { key: "{key}", value: child.clone(), depth: depth + 1, expanded } }
                        }
                    }
                }
            }
        }
    } else if let Some(entries) = value.as_array() {
        rsx! {
            Collapsible { open: open(), on_open_change: move |next: bool| open.set(next),
                CollapsibleTrigger {
                    size: ButtonSize::Sm,
                    class: Some("h-auto w-full justify-start gap-1 whitespace-normal px-0 py-0 text-left font-mono text-xs font-normal".into()),
                    Icon {
                        icon: if open() { IconType::ChevronDown } else { IconType::ChevronRight },
                        class: "size-3 shrink-0 text-muted-foreground",
                    }
                    "{entries.len()} items"
                }
                CollapsibleContent { class: Some("pl-3".into()),
                    ol { class: "ml-3 list-decimal",
                        for (index, child) in entries.iter().enumerate() {
                            li { span { "{index + 1}: " } JsonTree { key: "{index}", value: child.clone(), depth: depth + 1, expanded } }
                        }
                    }
                }
            }
        }
    } else {
        let display = value.to_string();
        rsx! { code { class: "break-all font-mono text-xs", "{display}" } }
    }
}
