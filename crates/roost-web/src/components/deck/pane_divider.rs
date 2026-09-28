//! One draggable seam between two split children: pointer capture, a
//! frame-coalesced live ratio, and the commit on release. `TerminalDeck`
//! supplies the geometry and receives the ratios. Ports
//! `apps/web/src/components/deck/PaneDivider.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::DividerRect;
use roost_protocol::layout::document::LayoutDirection;
use roost_protocol::layout::{LAYOUT_RATIO_MAX, LAYOUT_RATIO_MIN};

use super::inline_style::{InlineStyle, px};

/// The ratio a pointer at `position` (deck px along the divider's axis) means,
/// clamped to the shared document bounds; the middle when the region is empty.
pub fn divider_ratio(divider: &DividerRect, position: f64) -> f64 {
    let raw = if divider.region_len > 0.0 {
        (position - divider.region_start) / divider.region_len
    } else {
        0.5
    };
    raw.clamp(LAYOUT_RATIO_MIN, LAYOUT_RATIO_MAX)
}

/// The handle's style: its gutter box and the axis cursor.
pub fn divider_style(divider: &DividerRect) -> InlineStyle {
    let cursor = if divider.direction == LayoutDirection::Row {
        "col-resize"
    } else {
        "row-resize"
    };
    InlineStyle::new()
        .with("position", "absolute")
        .with("left", px(divider.rect.x))
        .with("top", px(divider.rect.y))
        .with("width", px(divider.rect.w))
        .with("height", px(divider.rect.h))
        .with("cursor", cursor)
        .with("z-index", "4")
        .with("touch-action", "none")
}

/// One divider.
#[component]
pub fn PaneDivider(
    divider: DividerRect,
    on_drag: EventHandler<(String, f64)>,
    on_commit: EventHandler<(String, f64)>,
) -> Element {
    let dragging = use_signal(|| false);
    let container = try_use_context::<super::deck_dom::DeckContainer>();
    #[cfg(target_arch = "wasm32")]
    let gesture: std::rc::Rc<
        std::cell::RefCell<Option<crate::motion::resize_pointer::PointerResizeHandle>>,
    > = use_hook(std::rc::Rc::default);
    #[cfg(target_arch = "wasm32")]
    let resize = use_context::<crate::motion::resize_drag::ResizeDrag>();
    #[cfg(target_arch = "wasm32")]
    {
        let gesture = std::rc::Rc::clone(&gesture);
        use_drop(move || {
            if let Some(handle) = gesture.borrow_mut().take() {
                handle.dispose();
            }
        });
    }
    let style = divider_style(&divider).css();
    rsx! {
        div {
            "data-testid": "pane-divider-{divider.split_id}",
            class: "pane-divider",
            "data-dir": divider.direction.as_wire().to_owned(),
            "data-dragging": dragging().then_some("true"),
            style,
            onpointerdown: move |event: PointerEvent| {
                if event.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary) {
                    return;
                }
                event.prevent_default();
                // The deck must not also read this press as a focus-pane click.
                event.stop_propagation();
                // Snapshotted at the press: the deck origin is fixed for the drag.
                let origin = container.map(|container| container.origin()).unwrap_or_default();
                #[cfg(target_arch = "wasm32")]
                begin_drag(&event, &gesture, resize, divider.clone(), (origin.left, origin.top), on_drag, on_commit, dragging);
                #[cfg(not(target_arch = "wasm32"))]
                let _ = (origin, on_drag, on_commit, dragging);
            },
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
fn begin_drag(
    event: &PointerEvent,
    gesture: &std::rc::Rc<
        std::cell::RefCell<Option<crate::motion::resize_pointer::PointerResizeHandle>>,
    >,
    resize: crate::motion::resize_drag::ResizeDrag,
    divider: DividerRect,
    deck_origin: (f64, f64),
    on_drag: EventHandler<(String, f64)>,
    on_commit: EventHandler<(String, f64)>,
    mut dragging: Signal<bool>,
) {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    use crate::motion::resize_pointer::{PointerResizeCallbacks, begin_pointer_resize_drag};

    if gesture.borrow().is_some() {
        return;
    }
    let Some(native) = event.try_as_web_event() else {
        return;
    };
    let native: web_sys::PointerEvent = native;
    let Some(target) = native
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    dragging.set(true);
    let split_for_move = divider.split_id.clone();
    let split_for_commit = divider.split_id.clone();
    let released = std::rc::Rc::clone(gesture);
    let geometry = divider.clone();
    let callbacks = PointerResizeCallbacks {
        geometry_for: Box::new(move |moved: &web_sys::PointerEvent| {
            let position = if geometry.direction == LayoutDirection::Row {
                f64::from(moved.client_x()) - deck_origin.0
            } else {
                f64::from(moved.client_y()) - deck_origin.1
            };
            divider_ratio(&geometry, position)
        }),
        on_move: Box::new(move |ratio| on_drag.call((split_for_move.clone(), ratio))),
        on_commit: Box::new(move |ratio| on_commit.call((split_for_commit.clone(), ratio))),
        on_release: Box::new(move || {
            released.borrow_mut().take();
            dragging.set(false);
        }),
    };
    *gesture.borrow_mut() = begin_pointer_resize_drag(
        resize,
        target,
        native.pointer_id(),
        divider.ratio,
        callbacks,
    );
}
