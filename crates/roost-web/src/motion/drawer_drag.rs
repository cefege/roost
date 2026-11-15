//! Drive the compact sidebar drawer under the finger, then hand its transform
//! back to the drawer's Tailwind `transform` utilities on settle. Ports
//! `apps/web/src/lib/drawerDrag.ts`; called by `MobileSidebarDrawer`'s edge
//! swipe and the deck's backward "workspace" swipe, which both move the SAME
//! drawer element (`[data-testid="sidebar-drawer"]`).
//!
//! Entering decelerates and leaving accelerates, from the motion tokens.

/// The drawer's `data-testid`, which is how both gestures find it.
pub const DRAWER_TEST_ID: &str = "sidebar-drawer";

/// The transition an entering drawer settles with.
pub const DECELERATE: &str = "transform 300ms cubic-bezier(0.05, 0.7, 0.1, 1)";
/// The transition a leaving drawer settles with.
pub const ACCELERATE: &str = "transform 200ms cubic-bezier(0.3, 0, 0.8, 0.15)";

/// Which gesture is settling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawerSettle {
    /// An open drag was released.
    Open,
    /// A close drag was released.
    Close,
}

/// The `(transition, transform)` a released drawer animates to: a committed
/// open or a cancelled close ends on-screen, the rest off the left edge.
pub fn settle_style(gesture: DrawerSettle, commit: bool) -> (&'static str, &'static str) {
    let ends_open = matches!(
        (gesture, commit),
        (DrawerSettle::Open, true) | (DrawerSettle::Close, false)
    );
    if ends_open {
        (DECELERATE, "translateX(0)")
    } else {
        (ACCELERATE, "translateX(-100%)")
    }
}

/// Settle deadline, including transitions that travel zero distance and fire no
/// `transitionend`. Every completion is fenced to the motion that scheduled it.
pub const HANDOFF_FALLBACK_MS: i32 = 350;

#[cfg(target_arch = "wasm32")]
mod dom {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::{JsValue, closure::Closure};
    use web_sys::HtmlElement;

    use super::{DRAWER_TEST_ID, DrawerSettle, HANDOFF_FALLBACK_MS, settle_style};

    const HANDOFF_OWNER: &str = "__roostDrawerHandoff";

    fn set_owner(element: &HtmlElement, owner: &JsValue) {
        let _ = js_sys::Reflect::set(element, &JsValue::from_str(HANDOFF_OWNER), owner);
    }

    fn drawer() -> Option<HtmlElement> {
        web_sys::window()?
            .document()?
            .query_selector(&format!("[data-testid=\"{DRAWER_TEST_ID}\"]"))
            .ok()
            .flatten()?
            .dyn_into::<HtmlElement>()
            .ok()
    }

    /// Follow the finger: `offset_px` is the drawer's `translateX`.
    pub fn drag_drawer(offset_px: f64) {
        let Some(element) = drawer() else { return };
        set_owner(&element, &JsValue::UNDEFINED);
        let style = element.style();
        let _ = style.set_property("transition", "none");
        let _ = style.set_property("transform", &format!("translateX({offset_px}px)"));
    }

    /// Animate the released drawer, then hand its transform back to CSS.
    pub fn settle_drawer(gesture: DrawerSettle, commit: bool) {
        let Some(element) = drawer() else { return };
        let Some(window) = web_sys::window() else {
            return;
        };
        let owner = JsValue::from(js_sys::Object::new());
        set_owner(&element, &owner);
        let (transition, transform) = settle_style(gesture, commit);
        let style = element.style();
        let _ = style.set_property("transition", transition);
        let _ = style.set_property("transform", transform);
        let completion = Closure::once_into_js(move || {
            let current = js_sys::Reflect::get(&element, &JsValue::from_str(HANDOFF_OWNER));
            if current.ok().as_ref() != Some(&owner) {
                return;
            }
            set_owner(&element, &JsValue::UNDEFINED);
            let style = element.style();
            let _ = style.set_property("transition", "none");
            let _ = style.remove_property("transform");
            // The drawer's CSS state must land before its transition returns.
            let _ = element.offset_width();
            let _ = style.remove_property("transition");
        });
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            completion.unchecked_ref(),
            HANDOFF_FALLBACK_MS,
        );
    }
}

#[cfg(target_arch = "wasm32")]
pub use dom::{drag_drawer, settle_drawer};
