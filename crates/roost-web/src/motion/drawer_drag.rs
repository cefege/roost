//! Drive the compact sidebar drawer under the finger, then hand its transform
//! back to the `.roost-drawer[data-open]` stylesheet on settle. Ports
//! `apps/web/src/lib/drawerDrag.ts`; called by `MobileSidebarDrawer`'s edge
//! swipe and the deck's backward "workspace" swipe, which both move the SAME
//! drawer element (`[data-testid="sidebar-drawer"]`).
//!
//! Entering decelerates and leaving accelerates, from the motion tokens.

/// The drawer's `data-testid`, which is how both gestures find it.
pub const DRAWER_TEST_ID: &str = "sidebar-drawer";

/// The transition an entering drawer settles with.
pub const DECELERATE: &str = "transform var(--md-sys-motion-duration-medium2, 300ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))";
/// The transition a leaving drawer settles with.
pub const ACCELERATE: &str = "transform var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-accelerate, cubic-bezier(0.3, 0, 0.8, 0.15))";

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

/// How long after a settle the inline transform is cleared even without a
/// `transitionend` (a zero-distance settle fires none).
pub const HANDOFF_FALLBACK_MS: i32 = 350;

#[cfg(target_arch = "wasm32")]
mod dom {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;
    use web_sys::HtmlElement;

    use super::{DRAWER_TEST_ID, DrawerSettle, HANDOFF_FALLBACK_MS, settle_style};

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
        let style = element.style();
        let _ = style.set_property("transition", "none");
        let _ = style.set_property("transform", &format!("translateX({offset_px}px)"));
    }

    /// Animate the released drawer, then hand its transform back to CSS.
    pub fn settle_drawer(gesture: DrawerSettle, commit: bool) {
        let Some(element) = drawer() else { return };
        let (transition, transform) = settle_style(gesture, commit);
        let style = element.style();
        let _ = style.set_property("transition", transition);
        let _ = style.set_property("transform", transform);
        let clear = {
            let element = element.clone();
            move || {
                let style = element.style();
                if style
                    .get_property_value("transform")
                    .unwrap_or_default()
                    .is_empty()
                {
                    return;
                }
                let _ = style.set_property("transition", "none");
                let _ = style.remove_property("transform");
                // A reflow here makes the snap back to the CSS position instant.
                let _ = element.offset_width();
                let _ = style.remove_property("transition");
            }
        };
        let on_end = Closure::once_into_js(clear.clone());
        let options = web_sys::AddEventListenerOptions::new();
        options.set_once(true);
        let _ = element.add_event_listener_with_callback_and_add_event_listener_options(
            "transitionend",
            on_end.unchecked_ref(),
            &options,
        );
        if let Some(window) = web_sys::window() {
            let fallback = Closure::once_into_js(clear);
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                fallback.unchecked_ref(),
                HANDOFF_FALLBACK_MS,
            );
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use dom::{drag_drawer, settle_drawer};
