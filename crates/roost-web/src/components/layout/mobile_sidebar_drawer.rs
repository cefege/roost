//! The compact sidebar drawer, its scrim, and the window touch listeners that
//! let a left-edge swipe open it and a leftward swipe close it. Ports
//! `apps/web/src/components/layout/MobileSidebarDrawer.tsx`; mounted by
//! `AppShell` only in the compact layout, with the sidebar surface as children.
//! The gesture is `drawer_gesture`; the drawer transforms are
//! `motion::drawer_drag`; open/close are the store's `SidebarIntent`s.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::pump::use_store;

/// The drawer and its scrim.
#[component]
pub fn MobileSidebarDrawer(children: Element) -> Element {
    let pump = use_store();
    let open = pump.core().borrow().store().ui.sidebar_open;
    #[cfg(target_arch = "wasm32")]
    {
        let listeners = use_hook({
            let pump = pump.clone();
            move || std::rc::Rc::new(touch::DrawerTouchListeners::install(pump))
        });
        use_drop(move || listeners.remove());
    }
    let close_pump = pump.clone();
    let flag = if open { "true" } else { "false" };
    rsx! {
        div {
            class: "roost-drawer-overlay",
            "data-testid": "sidebar-overlay",
            "data-open": flag,
            "aria-hidden": "true",
            onclick: move |_| close_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer)),
        }
        aside {
            class: "roost-drawer workbench-sidebar-drawer",
            "data-testid": crate::motion::drawer_drag::DRAWER_TEST_ID,
            "data-open": flag,
            "aria-hidden": if open { "false" } else { "true" },
            {children}
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod touch {
    use std::cell::RefCell;
    use std::rc::Rc;

    use roost_client_core::ClientEvent;
    use roost_client_core::store::sidebar::SidebarIntent;
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;
    use web_sys::TouchEvent;

    use crate::components::layout::drawer_gesture::{DrawerGesture, DrawerMove};
    use crate::motion::drawer_drag::{DrawerSettle, drag_drawer, settle_drawer};
    use crate::pump::Pump;

    type Listener = Closure<dyn FnMut(TouchEvent)>;

    /// The four capture-phase window listeners one mounted drawer holds.
    pub(super) struct DrawerTouchListeners {
        listeners: Vec<(&'static str, Listener)>,
    }

    fn now_ms() -> f64 {
        web_sys::window().and_then(|window| window.performance()).map_or(0.0, |clock| clock.now())
    }

    fn width() -> f64 {
        web_sys::window()
            .and_then(|window| window.inner_width().ok())
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0)
    }

    impl DrawerTouchListeners {
        pub(super) fn install(pump: Pump) -> Self {
            let gesture = Rc::new(RefCell::new(DrawerGesture::default()));
            let start = {
                let gesture = Rc::clone(&gesture);
                let pump = pump.clone();
                Listener::new(move |event: TouchEvent| {
                    let touches = event.touches();
                    let Some(touch) = touches.get(0) else {
                        gesture.borrow_mut().touch_start(0, 0.0, 0.0, false, false, now_ms());
                        return;
                    };
                    let excluded = event
                        .target()
                        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                        .and_then(|element| element.closest(".df-tab-bar, .df-row-swipe").ok().flatten())
                        .is_some();
                    let open = pump.core().borrow().store().ui.sidebar_open;
                    gesture.borrow_mut().touch_start(
                        touches.length(),
                        f64::from(touch.client_x()),
                        f64::from(touch.client_y()),
                        open,
                        excluded,
                        now_ms(),
                    );
                })
            };
            let moved = {
                let gesture = Rc::clone(&gesture);
                Listener::new(move |event: TouchEvent| {
                    let Some(touch) = event.touches().get(0) else { return };
                    let step = gesture.borrow_mut().touch_move(
                        f64::from(touch.client_x()),
                        f64::from(touch.client_y()),
                        width(),
                        now_ms(),
                    );
                    if let DrawerMove::Drag { offset_px } = step {
                        event.prevent_default();
                        event.stop_propagation();
                        drag_drawer(offset_px);
                    }
                })
            };
            let end = |gesture: Rc<RefCell<DrawerGesture>>, pump: Pump| {
                Listener::new(move |_event: TouchEvent| {
                    let Some((mode, commit)) = gesture.borrow_mut().touch_end(width(), now_ms()) else {
                        return;
                    };
                    if commit {
                        let intent = match mode {
                            DrawerSettle::Open => SidebarIntent::OpenDrawer,
                            DrawerSettle::Close => SidebarIntent::CloseDrawer,
                        };
                        pump.dispatch(ClientEvent::Sidebar(intent));
                    }
                    settle_drawer(mode, commit);
                })
            };
            let listeners = vec![
                ("touchstart", start),
                ("touchmove", moved),
                ("touchend", end(Rc::clone(&gesture), pump.clone())),
                ("touchcancel", end(gesture, pump)),
            ];
            if let Some(window) = web_sys::window() {
                for (name, listener) in &listeners {
                    let options = web_sys::AddEventListenerOptions::new();
                    options.set_capture(true);
                    options.set_passive(*name != "touchmove");
                    let _ = window.add_event_listener_with_callback_and_add_event_listener_options(
                        name,
                        listener.as_ref().unchecked_ref(),
                        &options,
                    );
                }
            }
            Self { listeners }
        }

        pub(super) fn remove(&self) {
            let Some(window) = web_sys::window() else { return };
            for (name, listener) in &self.listeners {
                let _ = window.remove_event_listener_with_callback_and_bool(
                    name,
                    listener.as_ref().unchecked_ref(),
                    true,
                );
            }
        }
    }
}
