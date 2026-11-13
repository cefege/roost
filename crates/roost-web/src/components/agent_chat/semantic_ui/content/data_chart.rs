//! Native bars, sparklines and heatmaps.
//! Each visualization maps source data through its declared value domain.
use super::data_tone::{tone_border, tone_fill, tone_stroke, tone_svg_fill};
use super::{prop_text, text_value};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

pub(super) fn render(node: &OmpUiNode) -> Element {
    if node.p.get("kind").and_then(Value::as_str) == Some("heatmap") {
        heatmap(node)
    } else {
        chart(node)
    }
}

fn chart(node: &OmpUiNode) -> Element {
    let series = node.p.get("series").and_then(Value::as_array);
    let declared_low = node.p.get("min").and_then(Value::as_f64);
    let low = declared_low.unwrap_or(0.0);
    let high = node
        .p
        .get("max")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| {
            series
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get("value").and_then(Value::as_f64))
                .fold(low, f64::max)
        });
    let span = (high - low).max(f64::EPSILON);
    let kind = prop_text(node, "kind");
    let size = prop_text(node, "size");
    let summary = prop_text(node, "summary");
    let token = prop_text(node, "token");
    let marks = node.p.get("marks").and_then(Value::as_array);
    let series_entries = series.into_iter().flatten().collect::<Vec<_>>();
    let height = match size.as_str() {
        "sm" => "h-8",
        "lg" => "h-20",
        _ => "h-12",
    };
    let section_class = if kind == "spark" {
        "flex min-w-0 flex-row items-center gap-2"
    } else {
        "flex min-w-0 flex-col gap-2"
    };
    rsx! {
        section { class: "{section_class}",
            if !summary.is_empty() { p { class: "text-sm text-muted-foreground", "{summary}" } }
            if kind == "spark" {
                {sparkline(&series_entries, low, span, marks, height, &token)}
            } else {
                div { class: "relative flex min-w-0 items-end gap-1 {height}", role: "img",
                    aria_label: if summary.is_empty() { "Bar chart".to_owned() } else { summary.clone() },
                    if let Some(marks) = marks {
                        for mark in marks {
                            { let value = mark.get("value").and_then(Value::as_f64).unwrap_or(low);
                              let y = 100.0 - ((value - low) / span).clamp(0.0, 1.0) * 100.0;
                              let title = mark.get("title").and_then(Value::as_str).unwrap_or("Reference");
                              let class = tone_border(mark.get("tone").and_then(Value::as_str), "muted");
                              rsx! { div { class: "absolute inset-x-0 z-10 border-t border-dashed {class}",
                                  style: "top: {y}%", title: "{title}: {value}", aria_hidden: "true"
                              } }
                            }
                        }
                    }
                    for entry in &series_entries { {chart_bar(entry, low, span, &token)} }
                }
                div { class: "flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground",
                    for entry in &series_entries {
                        { let label = entry.get("label").and_then(Value::as_str).unwrap_or("");
                          let value = entry.get("value").and_then(Value::as_f64).unwrap_or(0.0);
                          let title = entry.get("title").and_then(Value::as_str).unwrap_or(label);
                          rsx! { if !label.is_empty() { span { title: "{title}", "{label}: {value}" } } }
                        }
                    }
                }
            }
        }
    }
}

fn chart_bar(entry: &Value, low: f64, span: f64, token: &str) -> Element {
    let label = entry.get("label").and_then(Value::as_str).unwrap_or("");
    let value = entry.get("value").and_then(Value::as_f64).unwrap_or(0.0);
    let fraction = ((value - low) / span).clamp(0.0, 1.0);
    let fill = tone_fill(entry.get("tone").and_then(Value::as_str), token);
    let title = entry.get("title").and_then(Value::as_str).unwrap_or(label);
    rsx! {
        div { class: "flex h-full min-w-2 flex-1 items-end rounded-sm bg-muted",
            title: "{title}: {value}",
            span { class: "w-full rounded-sm {fill}", style: "height: {fraction * 100.0}%", aria_hidden: "true" }
        }
    }
}

fn sparkline(
    entries: &[&Value],
    low: f64,
    span: f64,
    marks: Option<&Vec<Value>>,
    height: &str,
    token: &str,
) -> Element {
    let count = entries.len();
    let points = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let value = entry.get("value")?.as_f64()?;
            let x = if count <= 1 {
                50.0
            } else {
                index as f64 * 100.0 / (count - 1) as f64
            };
            let y = 100.0 - ((value - low) / span).clamp(0.0, 1.0) * 100.0;
            Some(format!("{x:.2},{y:.2}"))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let last = entries
        .last()
        .and_then(|entry| entry.get("value"))
        .and_then(Value::as_f64);
    rsx! {
        svg {
            class: "w-full {height} overflow-visible",
            view_box: "0 0 100 100",
            preserve_aspect_ratio: "none",
            role: "img",
            "aria-label": "Sparkline from {low} to {low + span}",
            for mark in marks.into_iter().flatten() {
                { let value = mark.get("value").and_then(Value::as_f64).unwrap_or(low);
                  let y = 100.0 - ((value - low) / span).clamp(0.0, 1.0) * 100.0;
                  let title = mark.get("title").and_then(Value::as_str).unwrap_or("Reference");
                  let class = tone_stroke(mark.get("tone").and_then(Value::as_str), "muted");
                  rsx! { line { x1: "0", x2: "100", y1: "{y}", y2: "{y}",
                      class, "stroke-dasharray": "2 2", title { "{title}: {value}" }
                  } }
                }
            }
            polyline { points: "{points}", class: "fill-none {tone_stroke(None, token)}", "stroke-width": "3", vector_effect: "non-scaling-stroke" }
            if let Some(value) = last {
                circle { cx: if count <= 1 { "50".to_owned() } else { "100".to_owned() },
                    cy: "{100.0 - ((value - low) / span).clamp(0.0, 1.0) * 100.0}",
                    r: "3", class: "{tone_svg_fill(None, token)}", "aria-label": "Latest value {value}"
                }
            }
        }
    }
}

fn heatmap(node: &OmpUiNode) -> Element {
    let cells = node.p.get("cells").and_then(Value::as_array);
    let rows = node.p.get("rows").and_then(Value::as_array);
    let columns = node.p.get("cols").and_then(Value::as_array);
    let tips = node.p.get("tips").and_then(Value::as_array);
    let column_count = cells
        .into_iter()
        .flatten()
        .filter_map(|row| row.as_array().map(Vec::len))
        .max()
        .unwrap_or(0);
    let summary = prop_text(node, "summary");
    rsx! {
        section { class: "flex min-w-0 flex-col gap-2",
            if !summary.is_empty() { p { class: "text-sm text-muted-foreground", "{summary}" } }
            div { class: "overflow-x-auto",
                table { class: "w-full border-separate border-spacing-1 text-xs",
                    thead { tr {
                        th { "" }
                        for column_index in 0..column_count {
                            { let label = columns.and_then(|items| items.iter().find(|item|
                                item.get("at").and_then(Value::as_u64) == Some(column_index as u64)))
                                .and_then(|item| item.get("label")).map(text_value).unwrap_or_default();
                              rsx! { th { class: "text-left text-muted-foreground", "{label}" } }
                            }
                        }
                    } }
                    tbody {
                        for (row_index, row) in cells.into_iter().flatten().enumerate() {
                            { let row_label = rows.and_then(|items| items.get(row_index))
                                  .map(text_value).unwrap_or_else(|| (row_index + 1).to_string());
                              rsx! { tr {
                                  th { class: "pr-2 text-left font-normal text-muted-foreground", "{row_label}" }
                                  for (column_index, cell) in row.as_array().into_iter().flatten().enumerate() {
                                      { let value = cell.as_f64();
                                        let label = value.map(|value| format!("{:.0}%", value * 100.0))
                                            .unwrap_or_else(|| "No data".into());
                                        let title = tips.and_then(|items| items.get(row_index))
                                            .and_then(|items| items.get(column_index)).map(text_value)
                                            .unwrap_or_else(|| label.clone());
                                        let class = heat_class(value);
                                        rsx! { td { title: "{title}", aria_label: "{title}",
                                            span { class, role: "img", aria_label: "{label}" }
                                        } }
                                      }
                                  }
                              } }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn heat_class(value: Option<f64>) -> &'static str {
    match value {
        Some(value) if value >= 0.75 => "inline-block size-3 rounded-sm bg-primary",
        Some(value) if value >= 0.4 => "inline-block size-3 rounded-sm bg-primary/60",
        Some(_) => "inline-block size-3 rounded-sm bg-muted-foreground/40",
        None => "inline-block size-3 rounded-sm border border-border bg-muted",
    }
}
