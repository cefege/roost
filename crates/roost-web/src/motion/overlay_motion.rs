//! Enter motion for bespoke overlays that mount on a bare condition (command
//! palette, help overlay, docked consoles): a transform-only slide-up played on
//! mount through the Web Animations API. Ports
//! `apps/web/src/lib/overlayMotion.ts`; called by those overlays' `onmounted`.
//!
//! Exit is instant by design, and opacity is never animated, so a frozen
//! animation can never leave an overlay invisible.

/// Which geometry an overlay enters with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    /// A centred panel: slide up 8px and scale from 0.97.
    Panel,
    /// A bottom-right dock: slide up 12px and scale from 0.98.
    Dock,
}

impl OverlayKind {
    /// The transform the enter animation starts from.
    pub const fn from_transform(self) -> &'static str {
        match self {
            Self::Panel => "translateY(8px) scale(0.97)",
            Self::Dock => "translateY(12px) scale(0.98)",
        }
    }
}

/// The enter duration (the `medium2` motion token).
pub const OVERLAY_ENTER_MS: f64 = 300.0;
/// The enter easing (emphasized decelerate).
pub const EMPHASIZED_DECELERATE: &str = "cubic-bezier(0.05, 0.7, 0.1, 1)";

/// Play the enter animation on a freshly mounted overlay element.
#[cfg(target_arch = "wasm32")]
pub fn animate_overlay_enter(element: &web_sys::Element, kind: OverlayKind) {
    use wasm_bindgen::JsValue;

    if crate::motion::view_transition::prefers_reduced_motion() {
        return;
    }
    let frame = |transform: &str| {
        let keyframe = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&keyframe, &"transform".into(), &transform.into());
        JsValue::from(keyframe)
    };
    let keyframes = js_sys::Array::of2(&frame(kind.from_transform()), &frame("translateY(0) scale(1)"));
    let options = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&options, &"duration".into(), &OVERLAY_ENTER_MS.into());
    let _ = js_sys::Reflect::set(&options, &"easing".into(), &EMPHASIZED_DECELERATE.into());
    let _ = js_sys::Reflect::set(&options, &"fill".into(), &"backwards".into());
    let animate = js_sys::Reflect::get(element, &"animate".into())
        .ok()
        .and_then(|value| wasm_bindgen::JsCast::dyn_into::<js_sys::Function>(value).ok());
    if let Some(animate) = animate {
        let _ = animate.call2(element, &keyframes, &options);
    }
}
