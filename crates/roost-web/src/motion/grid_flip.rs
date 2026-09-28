//! FLIP move-animation for a grid whose children carry `data-flip-key`: after a
//! commit, each survivor slides from its old slot to its new one. Ports
//! `apps/web/src/lib/gridFlip.ts`; called by the settings machine grid.
//!
//! `flip_offset` is the decision; the wasm32 half measures and writes styles.

use std::collections::BTreeMap;

/// A measured slot: the left and top edges of one keyed child.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlipSlot {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
}

/// The measured slots of every keyed child, by key.
pub type FlipSlots = BTreeMap<String, FlipSlot>;

/// The default slide duration.
pub const FLIP_DURATION_MS: u32 = 250;

/// The inverted translation a survivor starts from, or `None` for a new child,
/// a child that did not move, or reduced motion.
pub fn flip_offset(
    first: Option<FlipSlot>,
    last: FlipSlot,
    reduced_motion: bool,
) -> Option<(f64, f64)> {
    let first = first.filter(|_| !reduced_motion)?;
    let (dx, dy) = (first.left - last.left, first.top - last.top);
    (dx != 0.0 || dy != 0.0).then_some((dx, dy))
}

/// Measure every keyed child of `container` and slide the survivors of `prev`
/// to their new slots; returns the new measurements for the next commit.
#[cfg(target_arch = "wasm32")]
pub fn flip_grid(container: &web_sys::Element, prev: &FlipSlots) -> FlipSlots {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let mut next = FlipSlots::new();
    let Ok(nodes) = container.query_selector_all("[data-flip-key]") else {
        return next;
    };
    let reduced = crate::motion::view_transition::prefers_reduced_motion();
    for index in 0..nodes.length() {
        let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            continue;
        };
        let Some(key) = element.dataset().get("flipKey") else {
            continue;
        };
        let rect = element.get_bounding_client_rect();
        let last = FlipSlot {
            left: rect.left(),
            top: rect.top(),
        };
        next.insert(key.clone(), last);
        let Some((dx, dy)) = flip_offset(prev.get(&key).copied(), last, reduced) else {
            continue;
        };
        let style = element.style();
        let _ = style.set_property("transition", "none");
        let _ = style.set_property("transform", &format!("translate({dx}px, {dy}px)"));
        let release = Closure::once_into_js(move || {
            let inner = Closure::once_into_js(move || {
                let style = element.style();
                let _ = style.set_property(
                    "transition",
                    &format!("transform {FLIP_DURATION_MS}ms var(--md-sys-motion-easing-emphasized, cubic-bezier(0.2,0,0,1))"),
                );
                let _ = style.remove_property("transform");
            });
            if let Some(window) = web_sys::window() {
                let _ = window.request_animation_frame(inner.unchecked_ref());
            }
        });
        if let Some(window) = web_sys::window() {
            let _ = window.request_animation_frame(release.unchecked_ref());
        }
    }
    next
}
