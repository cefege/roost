//! The compact sidebar drawer and the window touch listeners that
//! let a left-edge swipe open it and a leftward swipe close it. Ports
//! `apps/web/src/components/layout/MobileSidebarDrawer.tsx`; mounted by
//! `AppShell` only in the compact layout, with the sidebar surface as children.
//! The gesture is `drawer_gesture`; the drawer transforms are
//! `motion::drawer_drag`; open/close are the store's `SidebarIntent`s.

use dioxus::prelude::*;

use crate::pump::use_store;

/// The drawer; its dim is `main_pane`'s, which sits under it.
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
    let flag = if open { "true" } else { "false" };
    rsx! {
        aside {
            class: "fixed inset-0 z-50 box-border h-full w-screen overflow-hidden will-change-transform pt-[env(safe-area-inset-top)] pr-[env(safe-area-inset-right)] pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)] min-w-0 min-h-0 grid grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] [transform:translateX(-100%)] transition-transform duration-200 ease-[cubic-bezier(0.3,0,0.8,0.15)] data-[open=true]:[transform:translateX(0)] data-[open=true]:duration-300 data-[open=true]:ease-[cubic-bezier(0.05,0.7,0.1,1)]",
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

    use crate::components::deck::deck_dom::Listeners;
    use crate::components::layout::drawer_gesture::{DrawerGesture, DrawerMove};
    use crate::motion::drawer_drag::{DrawerSettle, drag_drawer, settle_drawer};
    use crate::motion::gesture_owner::overlay_owns_gesture;
    use crate::pump::Pump;

    type Listener = Closure<dyn FnMut(TouchEvent)>;
    type TouchCallbacks = Rc<RefCell<Vec<(&'static str, js_sys::Function)>>>;

    #[derive(Default)]
    struct Tracking {
        gesture: DrawerGesture,
        finger: Option<i32>,
        opened: bool,
    }

    impl Tracking {
        fn cancel(&mut self, pump: &Pump) {
            self.finger = None;
            if self.gesture.touch_cancel().is_some() {
                rollback(pump.core().borrow().store().ui.sidebar_open);
            }
        }
    }

    fn rollback(open: bool) {
        settle_drawer(
            if open {
                DrawerSettle::Close
            } else {
                DrawerSettle::Open
            },
            false,
        );
    }

    /// The four capture-phase window listeners one mounted drawer holds.
    pub(super) struct DrawerTouchListeners {
        listeners: Vec<(&'static str, Listener)>,
        follower: Rc<RefCell<Option<Listeners>>>,
        callbacks: TouchCallbacks,
        lifecycle: Closure<dyn FnMut(web_sys::Event)>,
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
            let tracking = Rc::new(RefCell::new(Tracking::default()));
            let follower: Rc<RefCell<Option<Listeners>>> = Rc::default();
            let callbacks: TouchCallbacks = Rc::default();
            let start = {
                let tracking = Rc::clone(&tracking);
                let pump = pump.clone();
                let follower = Rc::clone(&follower);
                let callbacks = Rc::clone(&callbacks);
                Listener::new(move |event: TouchEvent| {
                    let mut tracking = tracking.borrow_mut();
                    follower.borrow_mut().take();
                    tracking.cancel(&pump);
                    let touches = event.touches();
                    let Some(touch) = touches.get(0).filter(|_| touches.length() == 1) else {
                        return;
                    };
                    let excluded = event
                        .target()
                        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                        .and_then(|element| {
                            element
                                .closest("[data-tab-bar], [data-swipe-row], input, textarea, select, [contenteditable='true'], [role='slider']")
                                .ok()
                                .flatten()
                        })
                        .is_some()
                        || overlay_owns_gesture();
                    let open = pump.core().borrow().store().ui.sidebar_open;
                    let _ = tracking.gesture.touch_start(
                        touches.length(),
                        f64::from(touch.client_x()),
                        f64::from(touch.client_y()),
                        open,
                        excluded,
                        now_ms(),
                    );
                    tracking.finger = tracking.gesture.is_tracking().then_some(touch.identifier());
                    tracking.opened = open;
                    if tracking.finger.is_some() {
                        *follower.borrow_mut() = event.target().and_then(|target| {
                            let document = web_sys::window()?.document()?;
                            let callbacks = Rc::clone(&callbacks);
                            Listeners::follow_detached_touches(
                                target,
                                document.as_ref(),
                                move |event| {
                                    if let Some((_, callback)) = callbacks
                                        .borrow()
                                        .iter()
                                        .find(|(name, _)| *name == event.type_())
                                    {
                                        let _ = callback
                                            .call1(&wasm_bindgen::JsValue::UNDEFINED, &event);
                                    }
                                },
                            )
                        });
                    }
                })
            };
            let moved = {
                let tracking = Rc::clone(&tracking);
                let pump = pump.clone();
                Listener::new(move |event: TouchEvent| {
                    let mut tracking = tracking.borrow_mut();
                    if tracking.finger.is_none() {
                        return;
                    }
                    let touches = event.touches();
                    let touch = touches.get(0).filter(|touch| {
                        touches.length() == 1 && tracking.finger == Some(touch.identifier())
                    });
                    if overlay_owns_gesture()
                        || tracking.opened != pump.core().borrow().store().ui.sidebar_open
                        || touch.is_none()
                    {
                        tracking.cancel(&pump);
                        return;
                    }
                    let Some(touch) = touch else { return };
                    let step = tracking.gesture.touch_move(
                        f64::from(touch.client_x()),
                        f64::from(touch.client_y()),
                        width(),
                        now_ms(),
                    );
                    if !tracking.gesture.is_tracking() {
                        tracking.finger = None;
                    }
                    if let DrawerMove::Drag { offset_px } = step {
                        event.prevent_default();
                        event.stop_propagation();
                        drag_drawer(offset_px);
                    }
                })
            };
            let end = {
                let tracking = Rc::clone(&tracking);
                let pump = pump.clone();
                Listener::new(move |event: TouchEvent| {
                    let mut tracking = tracking.borrow_mut();
                    let released = event.changed_touches();
                    let touch = (0..released.length())
                        .filter_map(|index| released.get(index))
                        .find(|touch| tracking.finger == Some(touch.identifier()));
                    let Some(touch) = touch else { return };
                    if overlay_owns_gesture()
                        || tracking.opened != pump.core().borrow().store().ui.sidebar_open
                        || event.touches().length() != 0
                    {
                        tracking.cancel(&pump);
                        return;
                    }
                    let _ = tracking.gesture.touch_move(
                        f64::from(touch.client_x()),
                        f64::from(touch.client_y()),
                        width(),
                        now_ms(),
                    );
                    let result = tracking.gesture.touch_end(width(), now_ms());
                    tracking.finger = None;
                    drop(tracking);
                    if let Some((mode, commit)) = result {
                        if commit {
                            let intent = match mode {
                                DrawerSettle::Open => SidebarIntent::OpenDrawer,
                                DrawerSettle::Close => SidebarIntent::CloseDrawer,
                            };
                            pump.dispatch(ClientEvent::Sidebar(intent));
                        }
                        settle_drawer(mode, commit);
                    }
                })
            };
            let lifecycle = {
                let tracking = Rc::clone(&tracking);
                let pump = pump.clone();
                Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                    tracking.borrow_mut().cancel(&pump)
                })
            };
            let cancelled = {
                let tracking = Rc::clone(&tracking);
                Listener::new(move |_| tracking.borrow_mut().cancel(&pump))
            };
            let listeners = vec![
                ("touchstart", start),
                ("touchmove", moved),
                ("touchend", end),
                ("touchcancel", cancelled),
            ];
            *callbacks.borrow_mut() = listeners
                .iter()
                .skip(1)
                .map(|(name, listener)| {
                    (
                        *name,
                        listener
                            .as_ref()
                            .unchecked_ref::<js_sys::Function>()
                            .clone(),
                    )
                })
                .collect();
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
                for name in ["blur", "resize", "visibilitychange"] {
                    let _ = window.add_event_listener_with_callback_and_bool(
                        name,
                        lifecycle.as_ref().unchecked_ref(),
                        true,
                    );
                }
            }
            Self {
                listeners,
                follower,
                callbacks,
                lifecycle,
            }
        }

        pub(super) fn remove(&self) {
            self.follower.borrow_mut().take();
            self.callbacks.borrow_mut().clear();
            let Some(window) = web_sys::window() else {
                return;
            };
            for name in ["blur", "resize", "visibilitychange"] {
                let _ = window.remove_event_listener_with_callback_and_bool(
                    name,
                    self.lifecycle.as_ref().unchecked_ref(),
                    true,
                );
            }
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
