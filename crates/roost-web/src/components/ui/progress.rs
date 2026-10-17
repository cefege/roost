//! `Progress`: the linear bar for a transfer's progress or a machine metric's usage.
//! A known fraction fills the track; `None` runs the indeterminate sweep rather
//! than a bar that jumps to full. `MetricTile` and the transfer cards compose it.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The fill's inline width for a fraction, clamped so a byte count past its
/// declared total, or an over-committed resource, never draws outside the track.
pub fn progress_fill_width(fraction: f64) -> String {
    let percent = if fraction.is_finite() {
        (fraction * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    format!("width: {percent:.1}%;")
}

/// The bar. `label` names what it measures for assistive technology; `meter`
/// announces a level (a usage reading) rather than progress toward an end.
#[component]
pub fn Progress(
    value: Option<f64>,
    #[props(into)] label: String,
    #[props(default)] meter: bool,
    #[props(into, optional)] class: Option<String>,
) -> Element {
    let indeterminate = value.is_none();
    let now = value.map(|fraction| format!("{:.0}", (fraction * 100.0).clamp(0.0, 100.0)));
    let fill_style = value.map(progress_fill_width);
    let track_class = tw_merge!(
        "relative h-2 w-full overflow-hidden rounded-full bg-muted",
        class.as_deref().unwrap_or("")
    );
    let fill_class = if indeterminate {
        "h-full w-1/3 animate-pulse rounded-full bg-primary"
    } else {
        "h-full rounded-full bg-primary transition-all"
    };
    rsx! {
        div {
            class: "{track_class}",
            role: if meter { "meter" } else { "progressbar" },
            "aria-label": label,
            "aria-valuemin": "0",
            "aria-valuemax": "100",
            "aria-valuenow": now,
            "data-indeterminate": indeterminate.then_some("true"),
            div { class: "{fill_class}", style: fill_style }
        }
    }
}
