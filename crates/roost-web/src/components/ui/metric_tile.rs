//! `MetricTile`: one machine metric, its label, value, support line and an optional
//! usage bar. The machines and metrics surfaces compose it; the bar is `ui::Progress`
//! in meter mode, so the value is announced as a level.

use dioxus::prelude::*;

use crate::components::ui::{Icon, IconType, Progress};

/// A metric tile. `ratio` is 0..1; without one no bar is drawn.
#[component]
pub fn MetricTile(
    #[props(into)] label: String,
    icon: Option<IconType>,
    #[props(into)] value: String,
    #[props(into, optional)] support: Option<String>,
    ratio: Option<f64>,
) -> Element {
    let bar_label = label.clone();
    rsx! {
        div { class: "flex min-w-0 flex-col gap-1",
            span { class: "flex items-center gap-1.5 text-xs text-muted-foreground",
                if let Some(icon) = icon {
                    Icon { icon, class: "size-3.5" }
                }
                {label}
            }
            span { class: "text-lg font-semibold tabular-nums", {value} }
            if let Some(support) = support {
                span { class: "text-xs text-muted-foreground", {support} }
            }
            if let Some(ratio) = ratio {
                div { class: "mt-1",
                    Progress { value: Some(ratio), label: bar_label, meter: true }
                }
            }
        }
    }
}
