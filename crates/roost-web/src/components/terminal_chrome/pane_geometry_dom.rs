//! The browser half of the pane composer's growth measurement: the observer
//! that watches a laid-out dock, and the readings one update takes from it.
//!
//! Two observers, not one, and the reason is that they see different kinds of
//! change. A `ResizeObserver` sees the dock's box change size. A
//! `MutationObserver` sees the box's CONTENT change without its size changing —
//! a status line that appears, a caption whose text wraps to a second line —
//! and a composer that missed those is one whose field silently overlaps the
//! terminal as it grows.
//!
//! WHY A `Weak`, NOT A CYCLE. The observer callback has to reach the state it
//! updates, and the handle has to own the observer so it disconnects on
//! unmount. Capturing the `Rc` in the callback would make that a cycle and leak
//! both. A `Weak` breaks it: the handle holds the strong reference, and when
//! the pane unmounts the upgrade fails and the observer stops firing.
//! Ports the `attach`/`update` of
//! `apps/web/src/components/terminal/TerminalComposePaneGeometry.ts`; the
//! arithmetic it feeds is `super::pane_geometry`'s.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use dioxus::prelude::*;
use dioxus::web::WebEventExt as _;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use super::pane_geometry::PaneMeasurement;

/// What one update needs, captured so the callback can reach it.
struct GeometryState {
    dock: RefCell<Option<web_sys::Element>>,
    /// The box the resize observer is currently watching, so a remounted
    /// composer stops reporting the old one. The dock's own height is pinned
    /// to its resting row, so watching the dock alone can never see a draft
    /// grow — the box is the element that changes size.
    observed_box: RefCell<Option<web_sys::Element>>,
    /// The pane the resize observer is currently watching. The pane resizes
    /// independently of the fixed-height dock, and either can change whether
    /// the dock's content still fits.
    observed_pane: RefCell<Option<web_sys::Element>>,
    /// The resize observer, so one update can re-target it when the box or the
    /// pane element changes under a still-mounted dock.
    resize: RefCell<Option<web_sys::ResizeObserver>>,
    /// The resting height last written, so the property is only set when it
    /// moved. A style write on every frame is a layout invalidation per frame.
    published_rest: Cell<Option<i64>>,
    /// The growth last handed upward, de-duplicated for the same reason.
    published_growth: Cell<Option<u32>>,
    on_growth: RefCell<Callback<u32>>,
}

/// A pane dock under measurement.
///
/// Held by the composer for as long as the dock is mounted; dropping it stops
/// the observers, which is what makes the measurement stop with the pane rather
/// than with the page.
#[derive(Clone)]
pub struct PaneDockHandle {
    state: Rc<GeometryState>,
    observers: Rc<RefCell<Option<Observers>>>,
}

/// A `Debug` that names the handle without claiming its fields are printable.
///
/// A derive would demand `Debug` from `GeometryState` and `Observers`, and
/// `Observers` holds two `web-sys` handles that do not implement it — so the
/// derive would push a meaningless requirement into two types to satisfy a lint
/// on a type that exists to be handed to a DOM callback. What is useful about
/// this type in a log is that one exists and what it is attached to, and that is
/// all this says.
impl std::fmt::Debug for PaneDockHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneDockHandle")
            .field("attached", &self.observers.borrow().is_some())
            .finish_non_exhaustive()
    }
}

impl PartialEq for PaneDockHandle {
    /// Identity, not contents. Two handles are the same handle exactly when
    /// they are the same allocation, which is what a props comparison needs:
    /// every freshly-built handle compares unequal to every other, so a prop
    /// carrying one never re-renders a pane that kept its own.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.state, &other.state)
    }
}

/// The content observer, kept alive only while the dock is.
///
/// The field is never READ, and that is the point: a `MutationObserver` stops
/// observing the moment it is dropped, so holding it here is what keeps the
/// dock measuring. The callback cannot hold it — the JS function holds the
/// observer — so it is named and the name says why. The resize observer lives
/// in [`GeometryState`] instead, because one update has to re-target it.
#[allow(dead_code)]
struct Observers {
    mutation: web_sys::MutationObserver,
    _callback: Closure<dyn FnMut()>,
}

impl PaneDockHandle {
    /// A handle with nothing attached, so the composer can hold one before the
    /// dock exists rather than carrying an `Option` at every read.
    pub fn new() -> Self {
        Self {
            state: Rc::new(GeometryState {
                dock: RefCell::new(None),
                observed_box: RefCell::new(None),
                observed_pane: RefCell::new(None),
                resize: RefCell::new(None),
                published_rest: Cell::new(None),
                published_growth: Cell::new(None),
                on_growth: RefCell::new(Callback::new(|_| {})),
            }),
            observers: Rc::new(RefCell::new(None)),
        }
    }

    /// How far this pane must translate its display upward, in pixels.
    pub fn growth_px(&self) -> u32 {
        self.state.published_growth.get().unwrap_or(0)
    }

    /// Watch `dock` and measure it.
    ///
    /// A ref runs before its children are connected, so the box and the input
    /// do not exist at the moment the dock arrives. The composer therefore
    /// calls [`PaneDockHandle::refresh`] from an effect, which is the first
    /// pass where all three elements do.
    pub fn attach(&self, dock: Rc<MountedData>, on_growth: Callback<u32>) {
        let Some(element) = dock.try_as_web_event() else {
            return;
        };
        let dock: web_sys::Element = element;
        *self.state.dock.borrow_mut() = Some(dock.clone());
        *self.state.on_growth.borrow_mut() = on_growth;
        self.state.published_rest.set(None);
        self.state.published_growth.set(None);

        // The callback is stored beside the observers, because a `Closure` must
        // outlive the JS function it was converted into — dropping it while the
        // observer still holds that function is a use-after-free.
        let callback = Closure::wrap(Box::new({
            let weak: Weak<GeometryState> = Rc::downgrade(&self.state);
            move || {
                if let Some(state) = weak.upgrade() {
                    update_once(&state);
                }
            }
        }) as Box<dyn FnMut()>);
        let function = callback.as_ref().unchecked_ref();
        let Ok(resize) = web_sys::ResizeObserver::new(function) else {
            return;
        };
        let Ok(mutation) = web_sys::MutationObserver::new(function) else {
            return;
        };
        resize.observe(&dock);
        // Content changes the box's SIZE would not report: a status line that
        // appears, a caption whose text wraps to a second line.
        let watched = web_sys::MutationObserverInit::new();
        watched.set_child_list(true);
        watched.set_subtree(true);
        watched.set_character_data(true);

        if mutation.observe_with_options(&dock, &watched).is_err() {
            // A dock that refuses observation still has the resize observer,
            // so a size change is measured and only a content change is missed.
            tracing::debug!(target: "composer", "the pane dock refused content observation");
        }
        *self.state.resize.borrow_mut() = Some(resize);
        *self.observers.borrow_mut() = Some(Observers {
            mutation,
            _callback: callback,
        });
    }

    /// Measure now. Called from an effect, once the dock's children exist.
    pub fn refresh(&self) {
        update_once(&self.state);
    }
}

impl Default for PaneDockHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// One update: measure, publish the resting row, publish the growth.
fn update_once(state: &GeometryState) {
    let Some(dock) = state.dock.borrow().clone() else {
        return;
    };
    let box_element = query(&dock, ".term-chat__box");
    retarget_resize(state, box_element.clone(), dock.parent_element());
    let Some(measurement) = measure(&dock, box_element) else {
        return;
    };

    let rest = measurement.resting_height() as i64;
    if state.published_rest.get() != Some(rest) {
        state.published_rest.set(Some(rest));
        let _ = set_custom_property(&dock, "--term-chat-pane-rest", &format!("{rest}px"));
    }

    let _ = dock.set_attribute(
        "data-size-constrained",
        if measurement.constrained() {
            "true"
        } else {
            "false"
        },
    );

    let growth = measurement.publish().growth_px;
    if state.published_growth.get() != Some(growth) {
        state.published_growth.set(Some(growth));
        state.on_growth.borrow().call(growth);
    }
}

/// Point the resize observer at whatever the box and the pane are now.
///
/// The box is the element a growing draft resizes, and the dock is pinned to
/// its resting row, so an observer that watched only the dock would never see
/// the composer grow at all.
fn retarget_resize(
    state: &GeometryState,
    box_element: Option<web_sys::Element>,
    pane: Option<web_sys::Element>,
) {
    let previous_box = state.observed_box.borrow().clone();
    let previous_pane = state.observed_pane.borrow().clone();
    if box_element.as_ref() == previous_box.as_ref() && pane.as_ref() == previous_pane.as_ref() {
        return;
    }
    let borrowed = state.resize.borrow();
    let Some(resize) = borrowed.as_ref() else {
        return;
    };
    for stale in [previous_box, previous_pane].into_iter().flatten() {
        resize.unobserve(&stale);
    }
    for live in [box_element.as_ref(), pane.as_ref()].into_iter().flatten() {
        resize.observe(live);
    }
    drop(borrowed);
    *state.observed_box.borrow_mut() = box_element;
    *state.observed_pane.borrow_mut() = pane;
}

/// Read every measurement one update needs, or `None` when the dock is not laid
/// out yet.
fn measure(
    dock: &web_sys::Element,
    box_element: Option<web_sys::Element>,
) -> Option<PaneMeasurement> {
    let box_element = box_element?;
    let input = query(dock, ".term-chat__input")?;
    // A dock with no width is a dock that has not been laid out. Deliberately
    // not zero: a zero would un-grow a composer that is in fact tall.
    if dock.client_width() == 0 {
        return None;
    }
    let box_height = offset_height(&box_element)?;
    let input_height = offset_height(&input)?;
    if box_height <= 0.0 {
        return None;
    }

    let dock_rect = dock.get_bounding_client_rect();
    let box_rect = box_element.get_bounding_client_rect();
    let room = dock.parent_element().map_or(f64::INFINITY, |pane| {
        dock_rect.bottom() - pane.get_bounding_client_rect().top()
    });
    let content_height = dock_content_height(dock);

    Some(PaneMeasurement {
        box_height,
        field_growth: (input_height - computed_px(&input, "min-height")).max(0.0),
        overflow_above: dock_rect.top() - box_rect.top(),
        // The dock scrolls sideways, or its content is taller than the room it
        // has above the pane's top edge. Whether the resting row still fits
        // inside the dock's own height is the other half of the flag, and it is
        // decided by the target-independent half in `super::pane_geometry`.
        content_overflows: dock.scroll_width() > dock.client_width() + 1
            || content_height > room + 1.0,
        content_height,
        room_to_pane_top: room,
        dock_height: dock.client_height() as f64,
    })
}

/// The dock's content height: its element children plus their row gaps.
fn dock_content_height(dock: &web_sys::Element) -> f64 {
    let children = dock.children();
    let mut height = 0.0;
    for index in 0..children.length() {
        if let Some(child) = children.item(index)
            && let Some(height_px) = offset_height(&child)
        {
            height += height_px;
        }
    }
    height + computed_px(dock, "row-gap") * (children.length() as f64 - 1.0).max(0.0)
}

/// Write a custom property on a dock. `Element::set_property` is a CSSOM
/// escape hatch for unknown properties; the typed `style()` accessor rejects
/// them, so this is the one way to publish `--term-chat-pane-rest`.
fn set_custom_property(element: &web_sys::Element, name: &str, value: &str) -> Result<(), String> {
    let function = js_sys::Reflect::get(element, &JsValue::from_str("style"))
        .map_err(|_| "the dock's style is not readable".to_owned())?;
    let style: web_sys::CssStyleDeclaration = function
        .dyn_into()
        .map_err(|_| "the dock has no inline style".to_owned())?;
    style
        .set_property(name, value)
        .map_err(|error| format!("set {name} failed: {error:?}"))
}

fn query(root: &web_sys::Element, selector: &str) -> Option<web_sys::Element> {
    root.query_selector(selector).ok().flatten()
}

fn offset_height(element: &web_sys::Element) -> Option<f64> {
    element
        .dyn_ref::<web_sys::HtmlElement>()
        .map(|html| f64::from(html.offset_height()))
}

fn computed_px(element: &web_sys::Element, property: &str) -> f64 {
    let Some(document) = element.owner_document() else {
        return 0.0;
    };
    let Some(window) = document.default_view() else {
        return 0.0;
    };
    // A detached element has no computed style at all, which reads as an
    // absent value rather than an error; zero is the right answer for both a
    // missing `min-height` and a missing pane.
    let Ok(Some(computed)) = window.get_computed_style(element) else {
        return 0.0;
    };
    computed
        .get_property_value(property)
        .ok()
        .and_then(|value| {
            value
                .trim()
                .trim_end_matches("px")
                .trim()
                .parse::<f64>()
                .ok()
        })
        .unwrap_or(0.0)
}
