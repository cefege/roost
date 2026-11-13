//! Surface-scoped geometry and lifecycle for native floating overlays.
//! Uses shared fixed-containing-block and menu placement policy; listeners and
//! observers belong to the mounted overlay and leave with it.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use wasm_bindgen::{JsCast, closure::Closure};

use crate::components::ui::anchored_panel::fixed_containing_origin;

use super::super::overlay::{OverlayAnchor, OverlayShape, OverlaySize};
use super::super::{NativeViewScope, surface_view_id};
use super::geometry::place_configured_overlay;

#[path = "overlay_bounds.rs"]
mod bounds;
#[path = "overlay_caret.rs"]
mod caret;
use bounds::{anchor_bounds, element_box, intersects, visible_bounds};

type Listener = (web_sys::EventTarget, &'static str, Closure<dyn FnMut()>);

pub(super) fn use_overlay_placement(
    element_id: &str,
    surface_id: &str,
    shape: &OverlayShape,
    modal: bool,
) {
    let view = use_context::<NativeViewScope>();
    let watcher = use_hook(|| Rc::new(RefCell::new(None::<PlacementWatch>)));
    let element_id = element_id.to_owned();
    let surface_id = surface_id.to_owned();
    let shape = shape.clone();
    use_effect(use_reactive(
        (&element_id, &surface_id, &shape, &modal),
        move |(id, surface, shape, modal)| {
            *watcher.borrow_mut() =
                PlacementWatch::attach(&id, &view.view_id, &surface, shape, modal);
        },
    ));
}

struct PlacementWatch {
    listeners: Vec<Listener>,
    resize: web_sys::ResizeObserver,
    mutation: web_sys::MutationObserver,
    _on_resize: Closure<dyn FnMut()>,
    _on_mutation: Closure<dyn FnMut(js_sys::Array)>,
}

impl PlacementWatch {
    fn attach(
        id: &str,
        view_id: &str,
        surface: &str,
        shape: OverlayShape,
        modal: bool,
    ) -> Option<Self> {
        let window = web_sys::window()?;
        let document = window.document()?;
        let panel = document
            .get_element_by_id(id)?
            .dyn_into::<web_sys::HtmlElement>()
            .ok()?;
        let view = document
            .get_element_by_id(&format!("omp-view-{}", surface_view_id(view_id, surface)))?;
        let root = surface_root(&view, surface)?;
        let target_id = match &shape.anchor {
            OverlayAnchor::Node { id, .. } | OverlayAnchor::Caret { id } => Some(id.as_str()),
            _ => None,
        };
        let target = target_id.and_then(|id| node_target(&root, id));
        let update: Rc<dyn Fn()> = {
            let panel = panel.clone();
            let root = root.clone();
            let target_id = target_id.map(str::to_owned);
            Rc::new(move || {
                let target = target_id.as_deref().and_then(|id| node_target(&root, id));
                place_panel(&panel, &root, target.as_ref(), &shape, modal);
            })
        };
        let on_resize = {
            let update = update.clone();
            Closure::<dyn FnMut()>::new(move || update())
        };
        let resize = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()).ok()?;
        resize.observe(&panel);
        resize.observe(&root);
        if let Some(metrics) = panel
            .query_selector("[data-omp-overlay-metrics]")
            .ok()
            .flatten()
        {
            resize.observe(&metrics);
        }
        if let Some(target) = target {
            let mut ancestor = Some(target);
            while let Some(element) = ancestor {
                resize.observe(&element);
                if element == root {
                    break;
                }
                ancestor = element.parent_element();
            }
        }
        let on_mutation = {
            let update = update.clone();
            let panel = panel.clone();
            Closure::<dyn FnMut(js_sys::Array)>::new(move |records: js_sys::Array| {
                let relevant = records
                    .iter()
                    .filter_map(|record| record.dyn_into::<web_sys::MutationRecord>().ok())
                    .any(|record| {
                        let owns_style = record
                            .target()
                            .is_some_and(|target| target == web_sys::Node::from(panel.clone()))
                            && record.attribute_name().as_deref() == Some("style");
                        !owns_style
                    });
                if relevant {
                    update();
                }
            })
        };
        let mutation = web_sys::MutationObserver::new(on_mutation.as_ref().unchecked_ref()).ok()?;
        let options = web_sys::MutationObserverInit::new();
        options.set_subtree(true);
        options.set_child_list(true);
        options.set_character_data(true);
        options.set_attributes(true);
        mutation.observe_with_options(&root, &options).ok()?;
        let mut listeners = Vec::new();
        for (target, names) in [
            (
                web_sys::EventTarget::from(window.clone()),
                vec!["resize", "scroll"],
            ),
            (
                web_sys::EventTarget::from(document),
                vec![
                    "selectionchange",
                    "input",
                    "keyup",
                    "transitionend",
                    "animationend",
                ],
            ),
        ] {
            for name in names {
                attach_listener(&mut listeners, target.clone(), name, update.clone());
            }
        }
        if let Some(viewport) = window.visual_viewport() {
            for name in ["resize", "scroll"] {
                attach_listener(
                    &mut listeners,
                    viewport.clone().into(),
                    name,
                    update.clone(),
                );
            }
        }
        update();
        Some(Self {
            listeners,
            resize,
            mutation,
            _on_resize: on_resize,
            _on_mutation: on_mutation,
        })
    }
}

impl Drop for PlacementWatch {
    fn drop(&mut self) {
        self.resize.disconnect();
        self.mutation.disconnect();
        for (target, name, callback) in &self.listeners {
            let _ = target.remove_event_listener_with_callback_and_bool(
                name,
                callback.as_ref().unchecked_ref(),
                true,
            );
        }
    }
}

fn attach_listener(
    listeners: &mut Vec<Listener>,
    target: web_sys::EventTarget,
    name: &'static str,
    update: Rc<dyn Fn()>,
) {
    let callback = Closure::<dyn FnMut()>::new(move || update());
    let _ = target.add_event_listener_with_callback_and_bool(
        name,
        callback.as_ref().unchecked_ref(),
        true,
    );
    listeners.push((target, name, callback));
}

fn place_panel(
    panel: &web_sys::HtmlElement,
    root: &web_sys::Element,
    target: Option<&web_sys::Element>,
    shape: &OverlayShape,
    modal: bool,
) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(bounds) = visible_bounds(&window, root) else {
        return;
    };
    let anchored = matches!(
        shape.anchor,
        OverlayAnchor::Node { .. } | OverlayAnchor::Caret { .. }
    );
    let target_box = target.map(|target| {
        if matches!(shape.anchor, OverlayAnchor::Caret { .. }) {
            caret::caret_box(target).unwrap_or_else(|| element_box(target))
        } else {
            element_box(target)
        }
    });
    let target_bounds = target
        .map(|target| anchor_bounds(&window, target, bounds))
        .unwrap_or(bounds);
    let visible =
        !anchored || modal || target_box.is_some_and(|target| intersects(target, target_bounds));
    let style = panel.style();
    if !visible {
        let _ = style.set_property("visibility", "hidden");
        return;
    }
    let Some(metrics) = panel
        .query_selector("[data-omp-overlay-metrics]")
        .ok()
        .flatten()
    else {
        return;
    };
    let metrics = element_box(&metrics);
    let cell = (metrics.width, metrics.height);
    let rect = panel.get_bounding_client_rect();
    let fallback = if anchored && target_box.is_none() {
        OverlayAnchor::Center
    } else {
        shape.anchor.clone()
    };
    let target = target_box.unwrap_or(bounds);
    let measured = if shape.fullscreen {
        (bounds.width, bounds.height)
    } else {
        (
            rect.width(),
            f64::from(panel.scroll_height()).max(rect.height()),
        )
    };
    let mut placement =
        place_configured_overlay(&fallback, &shape.options, target, bounds, measured, cell);
    let explicit_width =
        shape.fullscreen || shape.options.width.is_some() || shape.options.min_width.is_some();
    if explicit_width {
        let _ = style.set_property("width", &format!("{}px", placement.width));
        let _ = style.set_property("max-width", &format!("{}px", placement.max_width));
        let measured = if shape.fullscreen {
            (placement.width, bounds.height)
        } else {
            (
                placement.width,
                f64::from(panel.scroll_height()).max(panel.get_bounding_client_rect().height()),
            )
        };
        placement =
            place_configured_overlay(&fallback, &shape.options, target, bounds, measured, cell);
    }
    let (origin_left, origin_top) = fixed_containing_origin(&panel.id()).unwrap_or_default();
    let max_width = if explicit_width {
        format!("{}px", placement.max_width)
    } else {
        match shape.size {
            OverlaySize::Sm => format!("min(var(--container-sm), {}px)", placement.max_width),
            OverlaySize::Md => format!("min(var(--container-lg), {}px)", placement.max_width),
            OverlaySize::Lg => format!("min(var(--container-3xl), {}px)", placement.max_width),
            OverlaySize::Full => format!("{}px", bounds.width),
        }
    };
    let width = if explicit_width {
        format!("{}px", placement.width)
    } else {
        "100%".to_owned()
    };
    for (property, value) in [
        ("position", "fixed".to_owned()),
        ("left", format!("{}px", placement.left - origin_left)),
        ("top", format!("{}px", placement.top - origin_top)),
        ("right", "auto".to_owned()),
        ("bottom", "auto".to_owned()),
        ("translate", "none".to_owned()),
        ("width", width),
        ("height", "auto".to_owned()),
        ("max-width", max_width),
        ("max-height", format!("{}px", placement.max_height)),
        ("visibility", "visible".to_owned()),
    ] {
        let _ = style.set_property(property, &value);
    }
    if shape.fullscreen {
        let _ = style.set_property("height", &format!("{}px", placement.max_height));
    }
}

fn surface_root(view: &web_sys::Element, surface: &str) -> Option<web_sys::Element> {
    if view.get_attribute("data-omp-surface").as_deref() == Some(surface) {
        return Some(view.clone());
    }
    let nodes = view.query_selector_all("[data-omp-surface]").ok()?;
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
        .find(|node| node.get_attribute("data-omp-surface").as_deref() == Some(surface))
}

fn node_target(root: &web_sys::Element, id: &str) -> Option<web_sys::Element> {
    let nodes = root.query_selector_all("[data-omp-node]").ok()?;
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
        .find(|node| node.get_attribute("data-omp-node").as_deref() == Some(id))
}
