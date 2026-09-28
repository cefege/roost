//! `MetricTile`: one machine metric — label, value, support line and an optional
//! usage bar. Ported from `apps/web/src/components/Settings/md/MetricTile.tsx`;
//! the machines and metrics surfaces compose it. `tokens.css` owns its look.

use dioxus::prelude::*;

use super::icon::{Icon, IconSize};

/// The bar's fill as a CSS width: the ratio as a percentage, clamped to the
/// track so an over-committed resource reads as full rather than overflowing.
pub fn metric_bar_width(ratio: f64) -> String {
    let percent = (ratio * 100.0).clamp(0.0, 100.0);
    format!("width: {percent}%;")
}

/// A metric tile. `ratio` is 0..1; without one no bar is drawn.
#[component]
pub fn MetricTile(
    label: String,
    icon: Option<String>,
    value: String,
    support: Option<String>,
    ratio: Option<f64>,
) -> Element {
    rsx! {
        div { class: "md-metric-tile",
            span { class: "md-metric-tile__label",
                if let Some(icon) = icon {
                    Icon { name: icon, size: IconSize::Sm }
                }
                {label}
            }
            span { class: "md-metric-tile__value", {value} }
            if let Some(support) = support {
                span { class: "md-metric-tile__support", {support} }
            }
            if let Some(ratio) = ratio {
                div { class: "md-metric-tile__bar",
                    span { style: metric_bar_width(ratio) }
                }
            }
        }
    }
}
