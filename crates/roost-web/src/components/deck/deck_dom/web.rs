//! The browser arm of `deck_dom`: element reads, timers and size observers,
//! over `web-sys`. The event listeners are in `web_listeners`.

use dioxus::prelude::*;
use dioxus::web::WebEventExt as _;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::{ClientBox, DeckPointerTarget, clips_rail};
use crate::components::deck::pane_strip_drag::TabRect;

pub(super) fn element(mounted: &MountedData) -> Option<web_sys::Element> {
    mounted.try_as_web_event()
}

/// The element's bounding box.
pub fn client_box(mounted: &MountedData) -> Option<ClientBox> {
    let element = element(mounted)?;
    Some(box_of(element.get_bounding_client_rect()))
}

fn box_of(rect: web_sys::DomRect) -> ClientBox {
    ClientBox {
        left: rect.left(),
        top: rect.top(),
        width: rect.width(),
        height: rect.height(),
    }
}

/// The element's content box (`clientWidth`, `clientHeight`).
pub fn client_size(mounted: &MountedData) -> Option<(f64, f64)> {
    let element = element(mounted)?;
    Some((
        f64::from(element.client_width()),
        f64::from(element.client_height()),
    ))
}

/// A custom property resolved on the element, read as `parseFloat` would;
/// 0 when unset or not a positive length.
pub fn css_px_var(mounted: &MountedData, name: &str) -> f64 {
    let Some(element) = element(mounted) else {
        return 0.0;
    };
    let value = web_sys::window()
        .and_then(|window| window.get_computed_style(&element).ok().flatten())
        .and_then(|style| style.get_property_value(name).ok())
        .unwrap_or_default();
    crate::components::deck::inline_style::parse_leading_px(&value)
        .filter(|px| *px > 0.0)
        .unwrap_or(0.0)
}

/// Every `.df-tab` box under a strip's rail, in DOM order.
pub fn tab_rects(rail: &MountedData) -> Vec<TabRect> {
    let Some(tabs) = element(rail).and_then(|rail| rail.query_selector_all(".df-tab").ok()) else {
        return Vec::new();
    };
    (0..tabs.length())
        .filter_map(|index| tabs.item(index)?.dyn_into::<web_sys::Element>().ok())
        .map(|tab| {
            let rect = tab.get_bounding_client_rect();
            TabRect::new(rect.left(), rect.width())
        })
        .collect()
}

/// Whether a tab is clipped by the rail's own box.
pub fn rail_overflowing(rail: &MountedData) -> bool {
    let Some(element) = element(rail) else {
        return false;
    };
    let bounds = box_of(element.get_bounding_client_rect());
    let Ok(tabs) = element.query_selector_all(".df-tab") else {
        return false;
    };
    (0..tabs.length()).any(|index| {
        tabs.item(index)
            .and_then(|tab| tab.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|tab| clips_rail(bounds, box_of(tab.get_bounding_client_rect())))
    })
}

/// Scroll the active tab into the rail's view.
pub fn reveal_active_tab(rail: &MountedData) {
    let Some(tab) = element(rail).and_then(|rail| {
        rail.query_selector(".df-tab[data-active='true']")
            .ok()
            .flatten()
    }) else {
        return;
    };
    let options = web_sys::ScrollIntoViewOptions::new();
    options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
    options.set_block(web_sys::ScrollLogicalPosition::Nearest);
    tab.scroll_into_view_with_scroll_into_view_options(&options);
}

/// Focus a tab's select button without scrolling the page.
pub fn focus_tab_select(session_id: &str) {
    let selector = format!("[data-testid=\"tab-{session_id}\"] .workbench-pane-tab__select");
    let Some(button) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector(&selector).ok().flatten())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let options = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &options,
        &"preventScroll".into(),
        &wasm_bindgen::JsValue::TRUE,
    );
    if let Ok(focus) = js_sys::Reflect::get(&button, &"focus".into())
        && let Ok(focus) = focus.dyn_into::<js_sys::Function>()
    {
        let _ = focus.call1(&button, &options);
    }
}

/// v2 `isTouchDevice`: a coarse pointer, or any touch points at all.
pub fn is_touch_device() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let coarse = window
        .match_media("(pointer: coarse)")
        .ok()
        .flatten()
        .is_some_and(|query| query.matches());
    coarse || window.navigator().max_touch_points() > 0
}

/// Whether the reader asked for reduced motion.
pub fn reduced_motion() -> bool {
    crate::motion::view_transition::prefers_reduced_motion()
}

/// A short haptic tick, where the device has one.
pub fn vibrate(duration_ms: u32) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().vibrate_with_duration(duration_ms);
    }
}

/// The viewport width.
pub fn viewport_width() -> f64 {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .unwrap_or(0.0)
}

/// Swallow the click a drag release would otherwise deliver to the tab.
pub fn swallow_next_click() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let swallow = Closure::once_into_js(|event: web_sys::Event| {
        event.stop_propagation();
        event.prevent_default();
    });
    let options = web_sys::AddEventListenerOptions::new();
    options.set_capture(true);
    options.set_once(true);
    let _ = window.add_event_listener_with_callback_and_add_event_listener_options(
        "click",
        swallow.unchecked_ref(),
        &options,
    );
}

/// Follow the finger with the workspace drawer.
pub fn drawer_follow(offset_px: f64) {
    crate::motion::drawer_drag::drag_drawer(offset_px);
}

/// Settle the workspace drawer open or back.
pub fn drawer_settle_open(commit: bool) {
    crate::motion::drawer_drag::settle_drawer(
        crate::motion::drawer_drag::DrawerSettle::Open,
        commit,
    );
}

/// Where a deck pointer-down landed.
pub fn deck_pointer_target(event: &PointerEvent) -> DeckPointerTarget {
    let target: Option<web_sys::Element> = event
        .try_as_web_event()
        .and_then(|native: web_sys::PointerEvent| native.target())
        .and_then(|target| target.dyn_into().ok());
    let Some(target) = target else {
        return DeckPointerTarget::default();
    };
    let closest = |selector: &str| target.closest(selector).ok().flatten();
    DeckPointerTarget {
        in_strip: closest("[data-pane-strip]").is_some(),
        pane_id: closest("[data-pane-id]").and_then(|pane| pane.get_attribute("data-pane-id")),
        in_link: closest("a").is_some(),
    }
}

/// The pane a focus arrival inside the phone chat input belongs to.
pub fn chat_input_focus_pane(event: &FocusEvent) -> Option<String> {
    let native: web_sys::FocusEvent = event.try_as_web_event()?;
    let target: web_sys::Element = native.target()?.dyn_into().ok()?;
    target
        .closest("[data-testid=\"mobile-chat-input\"]")
        .ok()
        .flatten()?;
    target
        .closest("[data-pane-id]")
        .ok()
        .flatten()?
        .get_attribute("data-pane-id")
}

/// A one-shot timer, cancelled when dropped.
pub struct Timeout {
    id: i32,
    _callback: Closure<dyn FnMut()>,
}

impl Timeout {
    /// Run `callback` after `delay_ms`.
    pub fn after(delay_ms: u32, callback: impl FnOnce() + 'static) -> Option<Self> {
        let mut callback = Some(callback);
        let closure = Closure::<dyn FnMut()>::new(move || {
            if let Some(callback) = callback.take() {
                callback();
            }
        });
        let id = web_sys::window()?
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                i32::try_from(delay_ms).unwrap_or(i32::MAX),
            )
            .ok()?;
        Some(Self {
            id,
            _callback: closure,
        })
    }
}

impl Drop for Timeout {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            window.clear_timeout_with_handle(self.id);
        }
    }
}

/// A `ResizeObserver` that calls back on any watched element's resize.
pub struct SizeWatch {
    observer: web_sys::ResizeObserver,
    _callback: Closure<dyn FnMut()>,
}

impl SizeWatch {
    /// An observer watching nothing yet.
    pub fn new(on_resize: impl FnMut() + 'static) -> Option<Self> {
        let callback = Closure::<dyn FnMut()>::new(on_resize);
        let observer = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()).ok()?;
        Some(Self {
            observer,
            _callback: callback,
        })
    }

    /// Watch one element.
    pub fn watch(&self, mounted: &MountedData) {
        if let Some(element) = element(mounted) {
            self.observer.observe(&element);
        }
    }

    /// Stop watching everything, then watch every `.df-tab` under `rail`.
    pub fn watch_tabs_only(&self, rail: &MountedData) {
        self.observer.disconnect();
        let Some(tabs) = element(rail).and_then(|rail| rail.query_selector_all(".df-tab").ok())
        else {
            return;
        };
        for index in 0..tabs.length() {
            if let Some(tab) = tabs
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                self.observer.observe(&tab);
            }
        }
    }
}

impl Drop for SizeWatch {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

impl std::fmt::Debug for Timeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Timeout")
            .field("id", &self.id)
            .finish()
    }
}

impl std::fmt::Debug for SizeWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SizeWatch").finish_non_exhaustive()
    }
}

/// Apply one key to the open menu `menu_id`: roving focus, activation,
/// Escape and Tab (`context_menu::run_menu_key`).
pub fn run_menu_keys(
    event: &KeyboardEvent,
    menu_id: &str,
    on_escape: impl FnOnce(),
    on_tab: impl FnOnce() + 'static,
) {
    let Some(native) = event.data().downcast::<web_sys::KeyboardEvent>().cloned() else {
        return;
    };
    let Some(menu) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(menu_id))
    else {
        return;
    };
    crate::components::context_menu::run_menu_key(&native, &menu, on_escape, on_tab);
}

/// Focus the first or last item of menu `menu_id` once it mounts.
pub fn focus_menu(menu_id: &str, edge: crate::components::context_menu::MenuFocusEdge) {
    crate::components::context_menu::focus_menu_edge(menu_id, edge);
}

/// Focus the element with `id`, if it is still in the document.
pub fn focus_by_id(id: &str) {
    if let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = element.focus();
    }
}
