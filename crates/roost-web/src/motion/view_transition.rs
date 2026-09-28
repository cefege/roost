//! Cross-fade a synchronous DOM mutation through the View Transitions API where
//! the browser has it and the reader has not asked for reduced motion; apply it
//! instantly otherwise. Ports `apps/web/src/browser/viewTransition.ts`; read by
//! the theme switch and the settings push/pop.
//!
//! Only SYNCHRONOUS mutations: the new snapshot is taken as `mutate` returns.

/// Whether the reader asked for reduced motion (`prefers-reduced-motion`).
#[cfg(target_arch = "wasm32")]
pub fn prefers_reduced_motion() -> bool {
    web_sys::window()
        .and_then(|window| window.match_media("(prefers-reduced-motion: reduce)").ok().flatten())
        .is_some_and(|query| query.matches())
}

/// Run `mutate` inside a view transition when one is available.
#[cfg(target_arch = "wasm32")]
pub fn with_view_transition(mutate: impl FnOnce() + 'static) {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let document = web_sys::window().and_then(|window| window.document());
    let start = document.as_ref().and_then(|document| {
        js_sys::Reflect::get(document, &"startViewTransition".into())
            .ok()
            .and_then(|value| value.dyn_into::<js_sys::Function>().ok())
    });
    match (document, start) {
        (Some(document), Some(start)) if !prefers_reduced_motion() => {
            let callback = Closure::once_into_js(mutate);
            if start.call1(&document, &callback).is_err() {
                tracing::warn!(target: "motion", "startViewTransition refused");
            }
        }
        _ => mutate(),
    }
}

/// A native build has no document to animate; the mutation applies at once.
#[cfg(not(target_arch = "wasm32"))]
pub fn with_view_transition(mutate: impl FnOnce() + 'static) {
    mutate();
}
