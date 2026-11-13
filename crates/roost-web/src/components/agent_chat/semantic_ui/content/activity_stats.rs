//! Nonclock agent facts shared by interactive and read-only rows.
//! Activity owns source status and timer components; this file formats only counters.
//! Text conversion follows the semantic content renderer's existing value policy.
use super::text_value;
use serde_json::Value;

pub(super) fn summary(stats: Option<&Value>) -> String {
    let Some(stats) = stats else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for (key, label) in [("tools", "tools"), ("requests", "requests")] {
        if let Some(value) = stats.get(key).filter(|value| !value.is_null()) {
            parts.push(format!("{} {label}", text_value(value)));
        }
    }
    if let Some(label) = stats.get("contextLabel").and_then(Value::as_str) {
        parts.push(format!("context {label}"));
    } else if let Some(value) = stats.get("context").and_then(Value::as_f64) {
        parts.push(format!("context {:.0}%", value * 100.0));
    } else if let Some(value) = stats.get("tokens").filter(|value| !value.is_null()) {
        match value.as_f64() {
            Some(count) => parts.push(format!("{} tok", compact_count(count))),
            None => parts.push(format!("{} tok", text_value(value))),
        }
    }
    if let Some(value) = stats.get("cost").and_then(Value::as_f64) {
        parts.push(if value >= 0.095 {
            format!("${value:.2}")
        } else {
            format!("${value:.3}")
        });
    }
    parts.join(" · ")
}

/// Compact K/M/B counts exactly as the terminal's `formatNumber` prints them.
fn compact_count(count: f64) -> String {
    let trim_one = |scaled: f64| {
        let text = format!("{scaled:.1}");
        text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
    };
    if count < 1_000.0 {
        return format!("{count}");
    }
    let (divisor, suffix) = if count < 1_000_000.0 {
        (1_000.0, "K")
    } else if count < 1_000_000_000.0 {
        (1_000_000.0, "M")
    } else {
        (1_000_000_000.0, "B")
    };
    let scaled = count / divisor;
    if scaled < 10.0 {
        format!("{}{suffix}", trim_one(scaled))
    } else {
        format!("{:.0}{suffix}", scaled)
    }
}
