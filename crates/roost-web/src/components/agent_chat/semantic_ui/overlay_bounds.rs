//! Client-space bounds a native overlay may occupy and anchor against.
//! `overlay_browser.rs` places panels with these; they read the visual
//! viewport, the surface root and every clipping ancestor of the anchor.

use crate::components::deck::deck_dom::ClientBox;

pub(super) fn element_box(element: &web_sys::Element) -> ClientBox {
    let rect = element.get_bounding_client_rect();
    ClientBox {
        left: rect.left(),
        top: rect.top(),
        width: rect.width(),
        height: rect.height(),
    }
}

pub(super) fn visible_bounds(
    window: &web_sys::Window,
    root: &web_sys::Element,
) -> Option<ClientBox> {
    let viewport = if let Some(viewport) = window.visual_viewport() {
        ClientBox {
            left: viewport.offset_left(),
            top: viewport.offset_top(),
            width: viewport.width(),
            height: viewport.height(),
        }
    } else {
        ClientBox {
            left: 0.0,
            top: 0.0,
            width: window.inner_width().ok()?.as_f64()?,
            height: window.inner_height().ok()?.as_f64()?,
        }
    };
    let root = element_box(root);
    let left = root.left.max(viewport.left);
    let top = root.top.max(viewport.top);
    let width = (root.right().min(viewport.right()) - left).max(0.0);
    let height = (root.bottom().min(viewport.bottom()) - top).max(0.0);
    Some(ClientBox {
        left,
        top,
        width,
        height,
    })
}

pub(super) fn intersects(target: ClientBox, bounds: ClientBox) -> bool {
    target.right() >= bounds.left
        && target.left <= bounds.right()
        && target.bottom() >= bounds.top
        && target.top <= bounds.bottom()
}

pub(super) fn anchor_bounds(
    window: &web_sys::Window,
    target: &web_sys::Element,
    mut bounds: ClientBox,
) -> ClientBox {
    let mut ancestor = Some(target.clone());
    while let Some(element) = ancestor {
        if let Some(style) = window.get_computed_style(&element).ok().flatten() {
            let rect = element_box(&element);
            for (property, vertical) in [("overflow-x", false), ("overflow-y", true)] {
                let clipped = style
                    .get_property_value(property)
                    .ok()
                    .is_some_and(|value| {
                        matches!(value.as_str(), "auto" | "scroll" | "hidden" | "clip")
                    });
                if clipped && vertical {
                    let top = bounds.top.max(rect.top);
                    bounds.height = (bounds.bottom().min(rect.bottom()) - top).max(0.0);
                    bounds.top = top;
                } else if clipped {
                    let left = bounds.left.max(rect.left);
                    bounds.width = (bounds.right().min(rect.right()) - left).max(0.0);
                    bounds.left = left;
                }
            }
        }
        ancestor = element.parent_element();
    }
    bounds
}
