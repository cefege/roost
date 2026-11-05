//! Applies native reveal and scroll requests to the painted surface.
//!
//! `OmpUiDocument` holds them only between a fold and the paint that consumes
//! them; this component drains its surface's queue once per revision, after
//! layout, and moves the nearest scroll container that owns the target node.
//! Native `end` on the surface history resumes follow; nothing here is replayed
//! after a reopen or a transport reset.

use crate::pump::Pump;
use dioxus::prelude::*;
#[cfg(any(target_arch = "wasm32", test))]
#[path = "scroll_geometry.rs"]
mod geometry;

#[cfg(target_arch = "wasm32")]
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiIntent, OmpUiRevealAt, OmpUiScrollBy};

#[component]
pub(super) fn NativeScrollEffects(
    pump: Pump,
    conversation_id: String,
    surface_id: String,
    view_id: String,
    revision: u64,
    on_follow: EventHandler<bool>,
) -> Element {
    #[cfg(target_arch = "wasm32")]
    use_effect(use_reactive((&revision,), move |_| {
        let Some(view) = view_scope(&view_id) else {
            return;
        };
        let pending = take_intents(&pump, &conversation_id, &surface_id);
        if pending.is_empty() {
            return;
        }
        // A view can hold an inline surface and a screen overlay at once, so
        // the anchors are narrowed to this surface before ids are compared.
        let scope = surface_scope(&view, &surface_id).unwrap_or(view);
        let anchors = NodeAnchors::under(&scope);
        for intent in pending {
            apply_intent(&anchors, intent, &on_follow);
        }
    }));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (
        pump,
        conversation_id,
        surface_id,
        view_id,
        revision,
        on_follow,
    );
    rsx! {}
}

/// The queue is drained only when it holds something, so an ordinary repaint
/// never takes a mutable borrow of the core.
#[cfg(target_arch = "wasm32")]
fn take_intents(pump: &Pump, conversation_id: &str, surface_id: &str) -> Vec<OmpUiIntent> {
    let pending = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .agent_chat
            .transcripts
            .get(conversation_id)
            .and_then(|loaded| loaded.transcript.omp_ui.as_ref())
            .and_then(|document| document.surfaces.get(surface_id))
            .is_some_and(|surface| !surface.intents().is_empty())
    };
    if !pending {
        return Vec::new();
    }
    pump.write_store(|store| {
        store
            .agent_chat
            .transcripts
            .get_mut(conversation_id)
            .and_then(|loaded| loaded.transcript.omp_ui.as_mut())
            .map(|document| document.take_intents(surface_id))
            .unwrap_or_default()
    })
}

#[cfg(target_arch = "wasm32")]
fn apply_intent(anchors: &NodeAnchors, intent: OmpUiIntent, on_follow: &EventHandler<bool>) {
    let Some(node) = anchors.get(intent.node_id()) else {
        return;
    };
    match intent {
        OmpUiIntent::Reveal { at, .. } => reveal(node, at),
        OmpUiIntent::Scroll { by, repeat, .. } => scroll(node, by, repeat, on_follow),
    }
}

/// Move the nearest container that scrolls either axis so the node lands at
/// `at` vertically and, only if needed, at the nearest inline position.
#[cfg(target_arch = "wasm32")]
fn reveal(node: &web_sys::Element, at: OmpUiRevealAt) {
    let Some(container) = nearest_container(node, Axis::Either) else {
        return;
    };
    let view = viewport_span(&container);
    let node_rect = node.get_bounding_client_rect();
    let block = view.block_scrolls.then(|| {
        geometry::reveal_shift(
            at,
            geometry::Span {
                start: node_rect.top(),
                end: node_rect.bottom(),
            },
            view.block,
        )
    });
    let inline = view.inline_scrolls.then(|| {
        geometry::inline_shift(
            geometry::Span {
                start: node_rect.left(),
                end: node_rect.right(),
            },
            view.inline,
        )
    });
    move_by(&container, inline.unwrap_or(0.0), block.unwrap_or(0.0));
}

/// Move the nearest vertically scrolling ancestor the native distance, and
/// report follow only when the surface history itself is the one moved.
#[cfg(target_arch = "wasm32")]
fn scroll(node: &web_sys::Element, by: OmpUiScrollBy, repeat: u32, on_follow: &EventHandler<bool>) {
    let Some(container) = nearest_container(node, Axis::Vertical) else {
        return;
    };
    let repeat = f64::from(repeat.max(1));
    match by {
        OmpUiScrollBy::Start => scroll_to(&container, 0.0),
        OmpUiScrollBy::End => scroll_to(&container, f64::from(container.scroll_height())),
        OmpUiScrollBy::LineUp
        | OmpUiScrollBy::LineDown
        | OmpUiScrollBy::PageUp
        | OmpUiScrollBy::PageDown => {
            let Some(line) = line_height(&container) else {
                return;
            };
            let page = geometry::page_step(f64::from(container.client_height()), line);
            let shift = geometry::direction_shift(by, line, page) * repeat;
            move_by(&container, 0.0, shift);
        }
    }
    if container.has_attribute("data-omp-history") {
        on_follow.call(by == OmpUiScrollBy::End);
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Vertical,
    Either,
}

#[cfg(target_arch = "wasm32")]
struct Viewport {
    block: geometry::Span,
    inline: geometry::Span,
    block_scrolls: bool,
    inline_scrolls: bool,
}

/// The visible band of a container plus which axes it actually scrolls, so a
/// reveal never writes an axis that only clips.
#[cfg(target_arch = "wasm32")]
fn viewport_span(container: &web_sys::Element) -> Viewport {
    let rect = container.get_bounding_client_rect();
    Viewport {
        block: geometry::Span {
            start: rect.top(),
            end: rect.bottom(),
        },
        inline: geometry::Span {
            start: rect.left(),
            end: rect.right(),
        },
        block_scrolls: container.scroll_height() > container.client_height(),
        inline_scrolls: container.scroll_width() > container.client_width(),
    }
}

#[cfg(target_arch = "wasm32")]
fn nearest_container(start: &web_sys::Element, axis: Axis) -> Option<web_sys::Element> {
    let window = web_sys::window()?;
    let mut current = Some(start.clone());
    while let Some(element) = current {
        if overflows(&window, &element, axis) {
            return Some(element);
        }
        current = element.parent_element();
    }
    None
}

/// Whether the element scrolls along `axis`: an overflow value the browser
/// scrolls on its own and content that actually exceeds the client box.
#[cfg(target_arch = "wasm32")]
fn overflows(window: &web_sys::Window, element: &web_sys::Element, axis: Axis) -> bool {
    let Some(style) = window.get_computed_style(element).ok().flatten() else {
        return false;
    };
    let block = scrollable_overflow(&property(&style, "overflow-y"))
        && element.scroll_height() > element.client_height();
    let inline = scrollable_overflow(&property(&style, "overflow-x"))
        && element.scroll_width() > element.client_width();
    match axis {
        Axis::Vertical => block,
        Axis::Either => block || inline,
    }
}

#[cfg(target_arch = "wasm32")]
fn scrollable_overflow(value: &str) -> bool {
    matches!(value, "auto" | "scroll" | "overlay")
}

/// One native line in pixels: the container's computed line box, else its font
/// size at the usual 1.5 ratio. `None` when neither is measurable.
#[cfg(target_arch = "wasm32")]
fn line_height(container: &web_sys::Element) -> Option<f64> {
    let style = web_sys::window()?
        .get_computed_style(container)
        .ok()
        .flatten()?;
    crate::components::deck::inline_style::parse_leading_px(&property(&style, "line-height"))
        .or_else(|| {
            crate::components::deck::inline_style::parse_leading_px(&property(&style, "font-size"))
                .map(|size| size * 1.5)
        })
        .filter(|value| *value > 0.0)
}

#[cfg(target_arch = "wasm32")]
fn property(style: &web_sys::CssStyleDeclaration, name: &str) -> String {
    style.get_property_value(name).unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
fn move_by(container: &web_sys::Element, inline: f64, block: f64) {
    if inline == 0.0 && block == 0.0 {
        return;
    }
    container.scroll_to_with_x_and_y(
        (f64::from(container.scroll_left()) + inline).max(0.0),
        (f64::from(container.scroll_top()) + block).max(0.0),
    );
}

/// Move along the block axis alone, leaving the inline position untouched.
#[cfg(target_arch = "wasm32")]
fn scroll_to(container: &web_sys::Element, block: f64) {
    container.scroll_to_with_x_and_y(f64::from(container.scroll_left()), block.max(0.0));
}

/// The painted subtree of one surface inside its view: the outermost element
/// that names it, or the view itself when the surface names no wrapper.
#[cfg(target_arch = "wasm32")]
fn surface_scope(view: &web_sys::Element, surface: &str) -> Option<web_sys::Element> {
    use wasm_bindgen::JsCast as _;
    if view.get_attribute("data-omp-surface").as_deref() == Some(surface) {
        return Some(view.clone());
    }
    let named = view.query_selector_all("[data-omp-surface]").ok()?;
    for index in 0..named.length() {
        let Some(element) = named
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        if element.get_attribute("data-omp-surface").as_deref() == Some(surface) {
            return Some(element);
        }
    }
    None
}

#[cfg(target_arch = "wasm32")]
fn view_scope(view_id: &str) -> Option<web_sys::Element> {
    if view_id.is_empty() {
        return None;
    }
    web_sys::window()?
        .document()?
        .get_element_by_id(&format!("omp-view-{view_id}"))
}

/// The node anchors this view painted, keyed by wire id. Scanning once per
/// drain keeps lookup independent of id contents (no selector escaping).
#[cfg(target_arch = "wasm32")]
struct NodeAnchors {
    items: Vec<(String, web_sys::Element)>,
}

#[cfg(target_arch = "wasm32")]
impl NodeAnchors {
    fn under(scope: &web_sys::Element) -> Self {
        use wasm_bindgen::JsCast as _;
        let mut items = Vec::new();
        if let Ok(anchors) = scope.query_selector_all("[data-omp-node]") {
            for index in 0..anchors.length() {
                let Some(element) = anchors
                    .item(index)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                else {
                    continue;
                };
                if let Some(id) = element.get_attribute("data-omp-node") {
                    items.push((id, element));
                }
            }
        }
        Self { items }
    }

    fn get(&self, id: &str) -> Option<&web_sys::Element> {
        self.items
            .iter()
            .find(|(anchor, _)| anchor.as_str() == id)
            .map(|(_, element)| element)
    }
}
