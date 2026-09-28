//! The desktop sidebar boundary: a focusable `separator` that resizes the
//! sidebar by pointer drag (captured, frame-coalesced), by arrow/Home/End, and
//! resets on double-click. Ports
//! `apps/web/src/components/layout/SidebarResizer.tsx`; mounted by
//! `SidebarRegion` while the sidebar is expanded. Widths are clamped by the
//! store (`store::ui::clamp_sidebar_width`) through `ShellIntent::SetSidebarWidth`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::ui::{SIDEBAR_WIDTH_DEFAULT, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN};

use crate::pump::use_store;

/// One keyboard step, in pixels.
pub const RESIZER_KEY_STEP_PX: u32 = 10;

/// The width a key asks for from `current`, or `None` when the key is not the
/// resizer's (the event is then left alone).
pub fn keyboard_width(key: &str, current: u32) -> Option<u32> {
    match key {
        "ArrowLeft" => Some(current.saturating_sub(RESIZER_KEY_STEP_PX)),
        "ArrowRight" => Some(current.saturating_add(RESIZER_KEY_STEP_PX)),
        "Home" => Some(SIDEBAR_WIDTH_MIN),
        "End" => Some(SIDEBAR_WIDTH_MAX),
        _ => None,
    }
}

/// The width a drag that started at `start_width` with the pointer at
/// `start_x` asks for with the pointer now at `x` (negative clamps to zero; the
/// store clamps to its range).
pub fn drag_width(start_width: u32, start_x: f64, x: f64) -> u32 {
    let width = f64::from(start_width) + (x - start_x);
    if width <= 0.0 { 0 } else { width.round() as u32 }
}

/// The resizer.
#[component]
pub fn SidebarResizer() -> Element {
    let pump = use_store();
    let width = pump.core().borrow().store().ui.sidebar_width;
    #[cfg(target_arch = "wasm32")]
    let drag = crate::motion::resize_drag::use_resize_drag();
    #[cfg(target_arch = "wasm32")]
    let mut active = use_signal(|| None::<crate::motion::resize_pointer::PointerResizeHandle>);
    #[cfg(target_arch = "wasm32")]
    use_drop(move || {
        if let Some(handle) = active.peek().as_ref() {
            handle.dispose();
        }
    });
    let key_pump = pump.clone();
    let reset_pump = pump.clone();
    #[cfg(target_arch = "wasm32")]
    let drag_pump = pump.clone();
    rsx! {
        div {
            class: "workbench-sidebar-resizer",
            "data-testid": "sidebar-resizer",
            role: "separator",
            "aria-label": "Resize sidebar",
            "aria-orientation": "vertical",
            "aria-valuemin": "{SIDEBAR_WIDTH_MIN}",
            "aria-valuemax": "{SIDEBAR_WIDTH_MAX}",
            "aria-valuenow": "{width}",
            tabindex: "0",
            onpointerdown: move |event: PointerEvent| {
                #[cfg(target_arch = "wasm32")]
                {
                    let busy = active.peek().is_some();
                    if let Some(handle) = pointer::begin(&event, busy, width, drag, drag_pump.clone(), active) {
                        active.set(Some(handle));
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                let _ = event;
            },
            onkeydown: move |event: KeyboardEvent| {
                if let Some(px) = keyboard_width(&event.key().to_string(), width) {
                    event.prevent_default();
                    key_pump.dispatch(ClientEvent::Shell(ShellIntent::SetSidebarWidth { px }));
                }
            },
            ondoubleclick: move |_| {
                reset_pump.dispatch(ClientEvent::Shell(ShellIntent::SetSidebarWidth { px: SIDEBAR_WIDTH_DEFAULT }));
            },
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod pointer {
    use dioxus::prelude::*;
    use dioxus::web::WebEventExt as _;
    use roost_client_core::ClientEvent;
    use roost_client_core::store::shell_intent::ShellIntent;
    use wasm_bindgen::JsCast as _;

    use super::drag_width;
    use crate::motion::resize_drag::ResizeDrag;
    use crate::motion::resize_pointer::{PointerResizeCallbacks, PointerResizeHandle, begin_pointer_resize_drag};
    use crate::pump::Pump;

    fn body_style(cursor: &str, user_select: &str) {
        if let Some(body) = web_sys::window().and_then(|window| window.document()).and_then(|document| document.body()) {
            let style = body.style();
            let _ = style.set_property("cursor", cursor);
            let _ = style.set_property("user-select", user_select);
        }
    }

    pub(super) fn begin(
        event: &PointerEvent,
        busy: bool,
        start_width: u32,
        drag: ResizeDrag,
        pump: Pump,
        mut active: Signal<Option<PointerResizeHandle>>,
    ) -> Option<PointerResizeHandle> {
        let native: web_sys::PointerEvent = event.try_as_web_event()?;
        if native.button() != 0 || busy {
            return None;
        }
        let target = native.current_target()?.dyn_into::<web_sys::Element>().ok()?;
        let start_x = f64::from(native.client_x());
        body_style("col-resize", "none");
        let set_width = move |pump: Pump| {
            move |px: u32| pump.dispatch(ClientEvent::Shell(ShellIntent::SetSidebarWidth { px }))
        };
        begin_pointer_resize_drag(
            drag,
            target,
            native.pointer_id(),
            start_width,
            PointerResizeCallbacks {
                geometry_for: Box::new(move |moved| drag_width(start_width, start_x, f64::from(moved.client_x()))),
                on_move: Box::new(set_width(pump.clone())),
                on_commit: Box::new(set_width(pump)),
                on_release: Box::new(move || {
                    active.set(None);
                    body_style("", "");
                }),
            },
        )
    }
}
