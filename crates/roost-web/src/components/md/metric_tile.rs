//! `MetricTile`: one machine metric — label, value, support line and an optional
//! usage bar. Ported from `apps/web/src/components/Settings/md/MetricTile.tsx`;
//! the machines and metrics surfaces compose it. `tokens.css` owns its look; the
//! bar is `ProgressBar`.

use dioxus::prelude::*;

use super::icon::{Icon, IconSize};
use super::progress_bar::ProgressBar;

/// A metric tile. `ratio` is 0..1; without one no bar is drawn.
#[component]
pub fn MetricTile(
    label: String,
    icon: Option<String>,
    value: String,
    support: Option<String>,
    ratio: Option<f64>,
) -> Element {
    let bar_label = label.clone();
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
                    ProgressBar { value: Some(ratio), label: bar_label, meter: true }
                }
            }
        }
    }
}
