//! Compact native meter and effort indicators.
//! Values, thresholds, marks and declared presentation styles drive the visuals.
use super::prop_text;
use crate::components::ui::{Progress, UsageRing};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

pub(super) fn render(node: &OmpUiNode) -> Element {
    match node.k.as_str() {
        "meter" => meter(node),
        "effort" => effort(node),
        _ => rsx! { span {} },
    }
}

/// The exact percentage an assistive technology should announce, clamped like
/// the drawn fill; an unknown or non-finite value announces nothing.
fn meter_percent(fraction: Option<f64>) -> Option<u32> {
    fraction
        .filter(|value| value.is_finite())
        .map(|value| (value.clamp(0.0, 1.0) * 100.0).round() as u32)
}

fn meter_value_text(fraction: Option<f64>, total: &str) -> Option<String> {
    let percent = meter_percent(fraction)?;
    Some(if total.is_empty() {
        format!("{percent}%")
    } else {
        format!("{percent}% of {total}")
    })
}

fn meter(node: &OmpUiNode) -> Element {
    let value = node.p.get("value").and_then(Value::as_f64);
    let fraction = value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0));
    let label = prop_text(node, "label");
    let total = prop_text(node, "total");
    let title = prop_text(node, "title");
    let style = prop_text(node, "style");
    let size = prop_text(node, "size");
    let thresholds = node.p.get("thresholds");
    let warning = thresholds
        .and_then(|item| item.get("warn"))
        .and_then(Value::as_f64);
    let critical = thresholds
        .and_then(|item| item.get("bad"))
        .and_then(Value::as_f64);
    let tone = fraction
        .map(|value| {
            if critical.is_some_and(|threshold| value >= threshold) {
                "text-destructive"
            } else if warning.is_some_and(|threshold| value >= threshold) {
                "text-warning"
            } else {
                "text-primary"
            }
        })
        .unwrap_or("text-muted-foreground");
    let accessible = if title.is_empty() {
        label.clone()
    } else {
        title
    };
    let marks = node.p.get("marks").and_then(Value::as_array);
    let parts = node.p.get("parts").and_then(Value::as_array);
    let compact =
        size == "sm" || node.p.get("role").and_then(Value::as_str) == Some("omp.composer.context");
    let percent = meter_percent(fraction);
    let value_text = meter_value_text(fraction, &total);
    rsx! {
        div {
            class: if compact { "inline-flex min-w-0 items-center gap-2" } else { "flex min-w-0 flex-col gap-1" },
            title: "{accessible}",
            if style == "ring" {
                UsageRing {
                    fraction,
                    label: Some(accessible.clone()),
                    title: Some(accessible.clone()),
                    size: match size.as_str() { "sm" => 16, "lg" => 24, _ => 20 }
                }
            } else if style == "blocks" {
                { let count = node.p.get("steps").and_then(Value::as_u64).unwrap_or(10).clamp(1, 24) as usize;
                  let filled = fraction.map(|value| (value * count as f64).round() as usize).unwrap_or(0);
                  rsx! { span { class: "inline-flex gap-0.5", role: "meter", aria_label: if accessible.is_empty() { "Usage meter".to_owned() } else { accessible.clone() },
                      aria_valuemin: "0", aria_valuemax: "100",
                      "aria-valuenow": percent.map(|value| value.to_string()),
                      "aria-valuetext": value_text.clone(),
                      for index in 0..count {
                          { let class = match (index < filled, tone) {
                              (true, "text-destructive") => "size-1.5 rounded-sm bg-destructive",
                              (true, "text-warning") => "size-1.5 rounded-sm bg-warning",
                              (true, "text-primary") => "size-1.5 rounded-sm bg-primary",
                              _ if index < filled => "size-1.5 rounded-sm bg-muted",
                              _ => "size-1.5 rounded-sm border border-border",
                          };
                            rsx! { span { class, aria_hidden: "true" } }
                          }
                      }
                  } }
                }
            } else if let Some(parts) = parts {
                div { class: "flex h-2 min-w-12 flex-1 overflow-hidden rounded-full bg-muted", role: "meter",
                    aria_label: if accessible.is_empty() { "Usage meter".to_owned() } else { accessible.clone() },
                    aria_valuemin: "0", aria_valuemax: "100",
                    "aria-valuenow": percent.map(|value| value.to_string()),
                    "aria-valuetext": value_text.clone(),
                    for part in parts {
                        { let amount = part.get("value").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0);
                          let name = part.get("label").and_then(Value::as_str)
                              .or_else(|| part.get("token").and_then(Value::as_str)).unwrap_or("Context");
                          let class = match part.get("hatch").and_then(Value::as_bool) {
                              Some(true) => "h-2 bg-primary/50",
                              _ => "h-2 bg-primary",
                          };
                          rsx! { span { class, style: "width: {amount * 100.0}%", title: "{name}", aria_hidden: "true" } }
                        }
                    }
                }
            } else {
                Progress {
                    value: fraction,
                    label: if accessible.is_empty() { "Usage meter".to_owned() } else { accessible.clone() },
                    meter: true,
                    class: if compact { "w-20" } else { "w-full" },
                }
            }
            if !compact && !label.is_empty() { span { class: "text-xs text-muted-foreground", "{label}" } }
            if let Some(value) = fraction { {let percent=value*100.0;rsx!{span { class: "text-xs tabular-nums {tone}", "{percent:.0}%" }}} }
            if !total.is_empty() { span { class: "text-xs text-muted-foreground", "/ {total}" } }
            if let Some(marks) = marks {
                for mark in marks {
                    { let at = mark.get("at").and_then(Value::as_f64).unwrap_or(0.0);
                      let mark_title = mark.get("title").and_then(Value::as_str).unwrap_or("Threshold");
                      let icon_name = mark.get("icon").and_then(Value::as_str).unwrap_or("");
                      let tone = mark.get("tone").and_then(Value::as_str).unwrap_or("muted");
                      let class = match tone {
                          "success" => "text-success",
                          "warning" => "text-warning",
                          "error" => "text-destructive",
                          _ => "text-muted-foreground",
                      };
                      let title = format!("{mark_title} at {:.0}%", at * 100.0);
                      let icon = super::data::icon_element(icon_name, "size-3");
                      rsx! {
                          span { class: "inline-flex size-2 items-center justify-center {class}",
                              title: "{title}", {icon}
                          }
                      }
                    }
                }
            }
        }
    }
}

fn effort(node: &OmpUiNode) -> Element {
    let level = prop_text(node, "level");
    let title = prop_text(node, "title");
    let levels = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    let active = levels.iter().position(|item| *item == level);
    let filled = active.unwrap_or(0);
    rsx! {
        span { class: "inline-flex items-center gap-2", title: "{title}",
            span { class: "inline-flex gap-0.5", role: "img", aria_label: "Thinking effort: {level}",
                for index in 0..levels.len() {
                    span {
                        class: if index < filled { "size-1.5 rounded-full bg-primary" } else { "size-1.5 rounded-full border border-border" },
                        aria_hidden: "true"
                    }
                }
            }
            span { class: "text-xs text-muted-foreground", "{level}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::meter_percent;

    #[test]
    fn announced_meter_percent_matches_the_drawn_fill() {
        assert_eq!(meter_percent(Some(0.42)), Some(42));
        assert_eq!(meter_percent(Some(1.5)), Some(100));
        assert_eq!(meter_percent(Some(-0.2)), Some(0));
        assert_eq!(meter_percent(Some(f64::NAN)), None);
        assert_eq!(meter_percent(None), None);
    }
}
