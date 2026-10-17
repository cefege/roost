//! `UsageRing`: the circular fill indicator for a context window or quota, read at
//! a glance. A known fraction fills clockwise from the top; `None` draws the bare
//! track. The outer `role="meter"` carries the exact numbers for hover and for
//! assistive technology; the `<svg>` is decorative.

use dioxus::prelude::*;

/// The diameter drawn when a caller names none, in CSS pixels.
pub const DEFAULT_USAGE_RING_SIZE_PX: u32 = 18;

/// The stroke width as a fraction of the diameter: thin enough to read as a ring
/// rather than a disc at the small sizes a header or a tab uses.
const STROKE_FRACTION: f64 = 0.18;

/// The tone a fill level paints with: calm under three quarters, a warning as it
/// closes in on the limit, the error tone once it all but fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRingTone {
    Calm,
    Warning,
    Error,
}

pub fn usage_ring_tone(fraction: f64) -> UsageRingTone {
    if fraction >= 0.92 {
        UsageRingTone::Error
    } else if fraction >= 0.75 {
        UsageRingTone::Warning
    } else {
        UsageRingTone::Calm
    }
}

/// The fill's stroke as a complete class, one literal per tone so Tailwind emits
/// each colour. The track and the empty ring use `stroke-border`.
fn usage_ring_fill_class(tone: UsageRingTone) -> &'static str {
    match tone {
        UsageRingTone::Calm => "stroke-primary",
        UsageRingTone::Warning => "stroke-warning",
        UsageRingTone::Error => "stroke-destructive",
    }
}

/// The ring's geometry for `size_px`: the stroke width, the path radius (centred
/// inside the stroke) and the circumference a dasharray walks.
pub fn ring_geometry(size_px: u32) -> (f64, f64, f64) {
    let size = f64::from(size_px);
    let stroke = (size * STROKE_FRACTION).max(1.5);
    let radius = (size - stroke) / 2.0;
    let circumference = std::f64::consts::TAU * radius;
    (stroke, radius, circumference)
}

/// How far the fill arc's dash offset sits for a clamped `fraction` of
/// `circumference`: `0` fills nothing, the circumference itself fills all of it.
pub fn usage_ring_offset(fraction: f64, circumference: f64) -> f64 {
    let clamped = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    circumference * (1.0 - clamped)
}

/// The ring. `fraction` is the share of a known limit; `None` draws the bare track.
#[component]
pub fn UsageRing(
    fraction: Option<f64>,
    #[props(default = DEFAULT_USAGE_RING_SIZE_PX)] size: u32,
    title: Option<String>,
) -> Element {
    let (stroke, radius, circumference) = ring_geometry(size);
    let center = f64::from(size) / 2.0;
    let fill_class = usage_ring_fill_class(fraction.map_or(UsageRingTone::Calm, usage_ring_tone));
    let offset = usage_ring_offset(fraction.unwrap_or(0.0), circumference);
    let dash = format!("{circumference:.2} {circumference:.2}");
    rsx! {
        span {
            class: "inline-flex",
            role: "meter",
            "aria-label": "Context window used",
            "aria-valuemin": "0",
            "aria-valuemax": "100",
            "aria-valuenow": fraction.map(|fraction| format!("{:.0}", fraction.clamp(0.0, 1.0) * 100.0)),
            title,
            svg {
                class: "shrink-0 -rotate-90",
                "aria-hidden": "true",
                width: "{size}",
                height: "{size}",
                view_box: "0 0 {size} {size}",
                circle {
                    class: "stroke-border",
                    cx: "{center}",
                    cy: "{center}",
                    r: "{radius}",
                    fill: "none",
                    "stroke-width": "{stroke}",
                }
                if fraction.is_some() {
                    circle {
                        class: "{fill_class}",
                        cx: "{center}",
                        cy: "{center}",
                        r: "{radius}",
                        fill: "none",
                        "stroke-width": "{stroke}",
                        "stroke-linecap": "round",
                        "stroke-dasharray": "{dash}",
                        "stroke-dashoffset": "{offset:.2}",
                    }
                }
            }
        }
    }
}
