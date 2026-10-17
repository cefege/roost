//! `Progress`: the linear bar for a transfer's progress or a machine metric's usage.
//! The fill's width is rust-ui's translate recipe. `MetricTile` and the transfer
//! cards compose it.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The fill's inline transform for a fraction, clamped so a byte count past its
/// declared total, or an over-committed resource, never draws outside the track.
pub fn progress_fill_transform(fraction: f64) -> String {
    let percent = if fraction.is_finite() {
        (fraction * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    format!("transform: translateX(-{}%)", 100.0 - percent)
}

/// Retained for callers that tag a transfer's state; the fill is rust-ui's
/// single `bg-primary` recipe, so every tone draws the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProgressTone {
    #[default]
    Default,
    Destructive,
    Warning,
}

/// The bar. `label` names what it measures for assistive technology; `meter`
/// announces a level (a usage reading) rather than progress toward an end.
/// `value: None` reads as an empty bar.
#[component]
pub fn Progress(
    value: Option<f64>,
    #[props(into)] label: String,
    #[props(default)] meter: bool,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] fill_test_id: Option<String>,
    #[props(default)] tone: ProgressTone,
) -> Element {
    let _ = tone;
    let fraction = value.unwrap_or(0.0);
    let now = format!("{:.0}", (fraction * 100.0).clamp(0.0, 100.0));
    let fill_style = progress_fill_transform(fraction);
    let merged = tw_merge!(
        "relative h-2 w-full overflow-hidden rounded-full bg-secondary",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "Progress",
            class: "{merged}",
            role: if meter { "meter" } else { "progressbar" },
            "aria-label": label,
            "aria-valuemin": "0",
            "aria-valuemax": "100",
            "aria-valuenow": "{now}",
            "data-testid": test_id,
            div {
                class: "flex-1 w-full h-full transition-all duration-300 ease-in-out bg-primary",
                style: "{fill_style}",
                "data-testid": fill_test_id,
            }
        }
    }
}
