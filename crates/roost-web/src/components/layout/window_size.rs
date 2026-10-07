//! The live window size class every layout decision reads: one signal, provided
//! once by `App`, fed by one frame-debounced resize/orientation listener. Ports
//! the reactive half of `apps/web/src/browser/windowSizeClass.ts` (the boundary
//! itself is `shell_metrics::classify`); read by `AppShell`, `MainPane`, the
//! deck and the sidebar through `use_is_compact`.
//!
//! Reactive, not measured once: a shell that read the viewport at mount kept its
//! desktop grid after the window crossed into compact, and the reverse.

use dioxus::prelude::*;

use super::shell_metrics::{SizeClass, classify};

/// The size class in context.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowSize {
    class: Signal<SizeClass>,
}

impl WindowSize {
    /// Provide the signal in the calling (root) scope and start listening.
    pub fn provide() -> Self {
        let size = use_context_provider(|| Self {
            class: Signal::new(current_class()),
        });
        #[cfg(target_arch = "wasm32")]
        use_hook(move || listen(size.class));
        size
    }
}

/// The current size class. Reading it subscribes the caller.
pub fn use_size_class() -> SizeClass {
    (use_context::<WindowSize>().class)()
}

/// v2 `isCompact()`, TV-aware: whether this is the compact (phone) shell
/// (`shell_metrics::is_compact_shell`).
pub fn use_is_compact() -> bool {
    let tv = use_tv_layout();
    super::shell_metrics::is_compact_shell(use_size_class(), tv)
}

/// Whether the ten-foot layout is on: TV mode from the user agent, `?tv=`, or
/// Settings → Theme. False before the document input installs the modality.
pub fn use_tv_layout() -> bool {
    try_use_context::<Signal<crate::input_nav::NavModality>>()
        .is_some_and(|modality| modality.read().tv_mode_active())
}

/// Classify the viewport as it is now.
fn current_class() -> SizeClass {
    let (width, height) = viewport();
    classify(width, height)
}

/// The viewport, in CSS pixels. A refused read is no viewport at all.
#[cfg(target_arch = "wasm32")]
fn viewport() -> (u32, u32) {
    web_sys::window().map_or((0, 0), |window| {
        let edge = |read: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
            read.ok()
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
                .max(0.0) as u32
        };
        (edge(window.inner_width()), edge(window.inner_height()))
    })
}

/// A native build has no viewport and classifies as desktop.
#[cfg(not(target_arch = "wasm32"))]
fn viewport() -> (u32, u32) {
    (1280, 900)
}

/// One frame-debounced listener for the life of the document; the closures are
/// leaked deliberately because the provider is the application root.
#[cfg(target_arch = "wasm32")]
fn listen(class: Signal<SizeClass>) {
    use std::cell::Cell;
    use std::rc::Rc;

    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let pending = Rc::new(Cell::new(false));
    let recompute = {
        let pending = Rc::clone(&pending);
        Closure::<dyn FnMut(f64)>::new(move |_| {
            pending.set(false);
            let next = current_class();
            let mut class = class;
            if *class.peek() != next {
                tracing::info!(target: "layout", class = ?next, "window size class changed");
                class.set(next);
            }
        })
    };
    let on_resize = {
        let window = window.clone();
        Closure::<dyn FnMut()>::new(move || {
            if !pending.replace(true) {
                let _ = window.request_animation_frame(recompute.as_ref().unchecked_ref());
            }
        })
    };
    for event in ["resize", "orientationchange"] {
        let _ = window.add_event_listener_with_callback(event, on_resize.as_ref().unchecked_ref());
    }
    on_resize.forget();
}
