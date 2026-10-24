//! The compact sidebar drawer and the window touch listeners that
//! let a left-edge swipe open it and a leftward swipe close it. Ports
//! `apps/web/src/components/layout/MobileSidebarDrawer.tsx`; mounted by
//! `AppShell` only in the compact layout, with the sidebar surface as children.
//! The gesture is `drawer_gesture`; the drawer transforms are
//! `motion::drawer_drag`; open/close are the store's `SidebarIntent`s.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;
use libero::{
    components::Flex,
    hooks::{DrawerOptions, ModalScope, use_drawer},
    sx::sx,
};
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::components::overlay_generation::OverlayGeneration;
use crate::pump::use_store;

/// The drawer; its dim is `main_pane`'s, which sits under it.
#[component]
pub fn MobileSidebarDrawer(children: Element) -> Element {
    let pump = use_store();
    let open = pump.core().borrow().store().ui.sidebar_open;
    let last_open = use_hook(|| Rc::new(Cell::new(false)));
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let drawer_sx = use_hook(|| {
        sx().padding("0")
            .width("100vw")
            .height("100dvh")
            .max_width("100vw")
    });
    let content_sx = use_hook(|| {
        sx().min_width("0")
            .min_height("0")
            .height("100%")
            .padding_top("env(safe-area-inset-top)")
            .padding_right("env(safe-area-inset-right)")
            .padding_bottom("env(safe-area-inset-bottom)")
            .padding_left("env(safe-area-inset-left)")
    });
    let drawer = use_drawer(
        DrawerOptions {
            anchor: "start".into(),
            size: "100vw".into(),
            aria_label: Some("Sidebar".to_owned()),
            sx: drawer_sx.into(),
            ..Default::default()
        },
        move |_scope: ModalScope<()>| {
            rsx! {
                Flex { sx: content_sx.clone(), direction: "column", wrap: false, {children.clone()} }
            }
        },
    );
    if open != last_open.get() {
        last_open.set(open);
        if open {
            let token = generation.borrow_mut().begin();
            let current_generation = generation.clone();
            let close_pump = pump.clone();
            drawer.open_with(()).onresult(move |_| {
                if current_generation.borrow_mut().finish(token) {
                    close_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));
                }
            });
        } else {
            generation.borrow_mut().invalidate();
            drawer.close();
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let listeners = use_hook({
            let pump = pump.clone();
            move || Rc::new(touch::DrawerTouchListeners::install(pump))
        });
        use_drop(move || listeners.remove());
    }
    rsx! {}
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
    use crate::motion::drawer_drag::DrawerSettle;
    use crate::pump::Pump;

    type Listener = Closure<dyn FnMut(TouchEvent)>;

    /// The four capture-phase window listeners one mounted drawer holds.
    pub(super) struct DrawerTouchListeners {
        listeners: Vec<(&'static str, Listener)>,
    }

    fn now_ms() -> f64 {
        web_sys::window()
            .and_then(|window| window.performance())
            .map_or(0.0, |clock| clock.now())
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
                        gesture
                            .borrow_mut()
                            .touch_start(0, 0.0, 0.0, false, false, now_ms());
                        return;
                    };
                    let excluded = event
                        .target()
                        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                        .and_then(|element| {
                            element
                                .closest("[data-tab-bar], [data-swipe-row]")
                                .ok()
                                .flatten()
                        })
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
                    let Some(touch) = event.touches().get(0) else {
                        return;
                    };
                    let step = gesture.borrow_mut().touch_move(
                        f64::from(touch.client_x()),
                        f64::from(touch.client_y()),
                        width(),
                        now_ms(),
                    );
                    if let DrawerMove::Drag { .. } = step {
                        event.prevent_default();
                        event.stop_propagation();
                    }
                })
            };
            let end = |gesture: Rc<RefCell<DrawerGesture>>, pump: Pump| {
                Listener::new(move |_event: TouchEvent| {
                    let Some((mode, commit)) = gesture.borrow_mut().touch_end(width(), now_ms())
                    else {
                        return;
                    };
                    if commit {
                        let intent = match mode {
                            DrawerSettle::Open => SidebarIntent::OpenDrawer,
                            DrawerSettle::Close => SidebarIntent::CloseDrawer,
                        };
                        pump.dispatch(ClientEvent::Sidebar(intent));
                    }
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
            let Some(window) = web_sys::window() else {
                return;
            };
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
