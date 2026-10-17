//! `UsageRing`: THE circular fill indicator — a context window or quota read
//! at a glance, the way a chat client shows it, rather than as a sentence of
//! numbers. A known fraction draws a ring that fills clockwise from the top;
//! an unknown one (`None`) draws an empty track. The exact numbers live in
//! `title`, read on hover or by assistive technology, so the ring itself is
//! `aria-hidden`.

use dioxus::prelude::*;

/// The diameter drawn when a caller names none, in CSS pixels.
pub const DEFAULT_USAGE_RING_SIZE_PX: u32 = 18;

/// The stroke width as a fraction of the diameter: thin enough to read as a
/// ring rather than a disc at the small sizes a header or a tab uses.
const STROKE_FRACTION: f64 = 0.18;

/// The token a fill level paints with: calm under three quarters, a warning
/// as it closes in on the limit, the error token once it all but fills.
pub fn usage_ring_token(fraction: f64) -> &'static str {
    if fraction >= 0.92 {
        "--status-err"
    } else if fraction >= 0.75 {
        "--status-warn"
    } else {
        "--md-primary"
    }
}

/// The ring's geometry for `size_px`: the path radius (centred inside the
/// stroke) and the circumference a dasharray walks.
fn ring_geometry(size_px: u32) -> (f64, f64, f64) {
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

/// The ring. `fraction` is read tokens over a known limit; `None` draws the
/// bare track, for a model whose context window is not in the catalog yet.
#[component]
pub fn UsageRing(
    fraction: Option<f64>,
    #[props(default = DEFAULT_USAGE_RING_SIZE_PX)] size: u32,
    title: Option<String>,
) -> Element {
    let (stroke, radius, circumference) = ring_geometry(size);
    let center = f64::from(size) / 2.0;
    let token = fraction.map(usage_ring_token).unwrap_or("--text-lo");
    let offset = usage_ring_offset(fraction.unwrap_or(0.0), circumference);
    let dash = format!("{circumference:.2} {circumference:.2}");
    rsx! {
        span {
            "aria-hidden": "true",
            title,
            style: "display: inline-flex;",
            svg {
                width: "{size}",
                height: "{size}",
                view_box: "0 0 {size} {size}",
                style: "flex-shrink: 0; transform: rotate(-90deg);",
                circle {
                    cx: "{center}",
                    cy: "{center}",
                    r: "{radius}",
                    fill: "none",
                    stroke: "var(--md-sys-color-outline-variant)",
                    "stroke-width": "{stroke}",
                }
                if fraction.is_some() {
                    circle {
                        cx: "{center}",
                        cy: "{center}",
                        r: "{radius}",
                        fill: "none",
                        stroke: "var({token})",
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

#[cfg(test)]
mod tests {
    use super::{ring_geometry, usage_ring_offset, usage_ring_token};

    #[test]
    fn the_token_steps_up_as_the_fill_approaches_the_limit() {
        assert_eq!(usage_ring_token(0.1), "--md-primary");
        assert_eq!(usage_ring_token(0.75), "--status-warn");
        assert_eq!(usage_ring_token(0.91), "--status-warn");
        assert_eq!(usage_ring_token(0.92), "--status-err");
        assert_eq!(usage_ring_token(1.5), "--status-err");
    }

    #[test]
    fn the_offset_runs_from_a_full_circumference_to_zero_and_clamps() {
        let (_, _, circumference) = ring_geometry(18);
        assert!((usage_ring_offset(0.0, circumference) - circumference).abs() < 0.001);
        assert!(usage_ring_offset(1.0, circumference).abs() < 0.001);
        let half = usage_ring_offset(0.5, circumference);
        assert!((half - circumference / 2.0).abs() < 0.001);
        // Over the limit still draws a full ring, not a negative dash offset.
        assert!(usage_ring_offset(1.4, circumference).abs() < 0.001);
        assert!((usage_ring_offset(f64::NAN, circumference) - circumference).abs() < 0.001);
    }

    #[test]
    fn geometry_keeps_a_readable_stroke_at_small_sizes() {
        let (stroke, radius, circumference) = ring_geometry(18);
        assert!(stroke >= 1.5);
        assert!(radius > 0.0);
        assert!(circumference > 0.0);
    }
}
