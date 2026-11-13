//! Browser measurement for a mounted native status strip.
//! `StatusWatch` in `status.rs` calls `measure` on resize and mutation; it sizes,
//! truncates and hides real segment elements by source priority and minimum.

use super::{Minimum, SegmentPolicy};
use wasm_bindgen::JsCast;

#[cfg(target_arch = "wasm32")]
struct MeasuredSegment {
    element: web_sys::HtmlElement,
    width: f64,
    natural_width: f64,
    minimum: f64,
    priority: f64,
    right: bool,
    visible: bool,
}

#[cfg(target_arch = "wasm32")]
pub(super) fn measure(root: &web_sys::HtmlElement, policies: &[SegmentPolicy]) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(computed) = window.get_computed_style(root).ok().flatten() else {
        return;
    };
    let pixels = |property: &str| {
        computed
            .get_property_value(property)
            .ok()
            .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
            .unwrap_or_default()
    };
    let available =
        (f64::from(root.client_width()) - pixels("padding-left") - pixels("padding-right"))
            .max(0.0);
    let Some(metrics) = root
        .query_selector("[data-omp-status-metrics]")
        .ok()
        .flatten()
    else {
        return;
    };
    let cell = metrics.get_bounding_client_rect();
    let Some(separator_probe) = root
        .query_selector("[data-omp-status-separator-probe]")
        .ok()
        .flatten()
    else {
        return;
    };
    let gap = separator_probe.get_bounding_client_rect().width();
    let elements = root.children();
    let mut segments = Vec::with_capacity(policies.len());
    for index in 0..elements.length() {
        let Some(element) = elements
            .item(index)
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            continue;
        };
        if element.has_attribute("data-omp-status-metrics")
            || element.has_attribute("data-omp-status-separator-probe")
            || element.has_attribute("data-omp-status-separator")
        {
            continue;
        }
        let Some(policy) = policies.get(segments.len()) else {
            break;
        };
        // Hidden segments are measured in the same DOM, synchronously before paint;
        // no cloned text can enter selection, accessible names, or node actions.
        element.set_hidden(false);
        let _ = element.style().remove_property("display");
        for (property, value) in [
            ("width", "max-content"),
            ("max-width", "none"),
            ("min-width", "0"),
            ("flex-grow", "0"),
            ("flex-shrink", "0"),
            ("flex-basis", "auto"),
            ("margin-left", "0"),
            ("overflow-x", "hidden"),
            ("overflow-y", "hidden"),
        ] {
            set_style(&element, property, value);
        }
        let width = element.get_bounding_client_rect().width();
        let minimum = policy
            .minimum
            .as_ref()
            .map(|minimum| match minimum {
                Minimum::Fraction(fraction) => available * fraction,
                Minimum::Characters(count) => cell.width() * count,
                Minimum::Lines(count) => cell.height() * count,
            })
            .unwrap_or(width)
            .min(width)
            .max(0.0);
        segments.push(MeasuredSegment {
            element,
            width,
            natural_width: width,
            minimum,
            priority: policy.priority,
            right: policy.right,
            visible: true,
        });
    }
    fit(&mut segments, available, gap);
    let visible_count = segments.iter().filter(|segment| segment.visible).count();
    let separators = root
        .query_selector_all(":scope > [data-omp-status-separator]")
        .ok();
    let mut right_started = false;
    let mut visible_index = 0;
    for (index, segment) in segments.iter().enumerate() {
        let element = &segment.element;
        element.set_hidden(!segment.visible);
        if segment.visible {
            let _ = element.remove_attribute("aria-hidden");
            let _ = element.style().remove_property("display");
            set_style(element, "width", &format!("{}px", segment.width));
            set_style(element, "max-width", "100%");
            let first_right = segment.right && !right_started;
            visible_index += 1;
            right_started |= segment.right;
            set_style(
                element,
                "margin-left",
                if first_right { "auto" } else { "0" },
            );
        } else {
            let _ = element.set_attribute("aria-hidden", "true");
            set_style(element, "display", "none");
        }
        if let Some(separator) = separators
            .as_ref()
            .and_then(|separators| separators.item(index as u32))
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let visible = segment.visible && visible_index < visible_count;
            separator.set_hidden(!visible);
            set_style(
                &separator,
                "display",
                if visible { "block" } else { "none" },
            );
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn fit(segments: &mut [MeasuredSegment], available: f64, gap: f64) {
    let mut remaining = segments.len();
    let mut used = segments.iter().map(|segment| segment.width).sum::<f64>()
        + gap * remaining.saturating_sub(1) as f64;
    // Explicit minima opt into truncation before a whole segment is dropped.
    for segment in segments.iter_mut() {
        if used <= available {
            break;
        }
        let shrink = (segment.width - segment.minimum).min(used - available);
        segment.width -= shrink;
        used -= shrink;
    }
    while used > available && remaining > 1 {
        let drop_index = segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| segment.visible)
            .min_by(|(left_index, left), (right_index, right)| {
                left.priority
                    .total_cmp(&right.priority)
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(index, _)| index);
        let Some(drop_index) = drop_index else {
            break;
        };
        let segment = &mut segments[drop_index];
        segment.visible = false;
        used -= segment.width + gap;
        remaining -= 1;
    }
    for segment in segments.iter_mut().filter(|segment| segment.visible) {
        let growth = (segment.natural_width - segment.width).min((available - used).max(0.0));
        segment.width += growth;
        used += growth;
        if remaining == 1 {
            segment.width = segment.natural_width.min(available);
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn set_style(element: &web_sys::HtmlElement, property: &str, value: &str) {
    let style = element.style();
    if style.get_property_value(property).ok().as_deref() != Some(value) {
        let _ = style.set_property(property, value);
    }
}
