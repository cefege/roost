//! The browser clock the terminal components' timers read: wall time, page
//! visibility, and an awaitable delay. Called by `terminal_startup_overlay`;
//! the native arms answer a fixed clock and never resolve a delay, because the
//! native target renders nothing to animate. Ports the `setTimeout` /
//! `performance.now()` reads of `apps/web/src/components/terminal/TerminalStartupOverlay.tsx`.

/// Milliseconds since the Unix epoch.
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> u64 {
    js_sys::Date::now().max(0.0) as u64
}

/// Milliseconds since the Unix epoch.
#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u64 {
    0
}

/// Whether the document is visible.
#[cfg(target_arch = "wasm32")]
pub fn page_visible() -> bool {
    crate::platform::visibility::page_visible()
}

/// Whether the document is visible.
#[cfg(not(target_arch = "wasm32"))]
pub fn page_visible() -> bool {
    crate::platform::visibility::page_visible()
}

/// Resolve after `delay_ms`.
#[cfg(target_arch = "wasm32")]
pub async fn sleep_ms(delay_ms: u64) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let delay = i32::try_from(delay_ms).unwrap_or(i32::MAX);
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, delay);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Resolve after `delay_ms`: never, natively, where nothing animates.
#[cfg(not(target_arch = "wasm32"))]
pub async fn sleep_ms(_delay_ms: u64) {
    std::future::pending::<()>().await;
}
