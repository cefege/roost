//! `ProgressBar`: THE linear bar — a transfer's progress, a machine metric's
//! usage. Used by the transfer cards and `MetricTile`, and rendered on
//! `/design`. A known fraction fills the track; an unknown one (`None`) runs
//! the indeterminate sweep rather than a bar that jumps to full. `controls.css`
//! owns its look, so the browser's own `<progress>` chrome never shows through.

use dioxus::prelude::*;

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
pub fn ProgressBar(value: Option<f64>, label: String, #[props(default)] meter: bool) -> Element {
    let indeterminate = value.is_none();
    let now = value.map(|fraction| format!("{:.0}", (fraction * 100.0).clamp(0.0, 100.0)));
    let fill = value.map(progress_fill_width);
    rsx! {
        div {
            class: "roost-progress",
            role: if meter { "meter" } else { "progressbar" },
            "aria-label": label,
            "aria-valuemin": "0",
            "aria-valuemax": "100",
            "aria-valuenow": now,
            "data-indeterminate": indeterminate.then_some("true"),
            div { class: "roost-progress__fill", style: fill }
        }
    }
}
