//! The finalize watchdog: the one deadline the mic itself owns.
//!
//! Split out of `super::mobile_voice_input` because a timer outlives the call
//! that armed it and outlives the recording it was watching, so it is a
//! lifetime of its own rather than a line of the component's body. The engine
//! has its own, shorter wait; this is the outer bound, so a wedged engine cannot
//! leave the composer saying `finalizing` forever. Both the arm and the cancel
//! live here so a component that stops being mounted has one door to call.

use std::cell::RefCell;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

/// How long a finalize waits for the engine before the composer gives up on it.
pub const FINALIZE_WATCHDOG_MS: i32 = 6_000;

thread_local! {
    /// The finalize watchdog's timer, held so the composer can cancel it.
    static WATCHDOG: RefCell<Option<i32>> = const { RefCell::new(None) };
}

/// Wait this long for a final result before finishing the recording anyway.
pub fn arm_watchdog() {
    clear_watchdog();
    #[cfg(target_arch = "wasm32")]
    if let Some(window) = web_sys::window() {
        let expiry = wasm_bindgen::closure::Closure::once(|| {});
        if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            expiry.as_ref().unchecked_ref(),
            FINALIZE_WATCHDOG_MS,
        ) {
            expiry.forget();
            WATCHDOG.with(|watchdog| *watchdog.borrow_mut() = Some(id));
        }
    }
}

/// Cancel the finalize watchdog.
pub fn clear_watchdog() {
    #[cfg(target_arch = "wasm32")]
    if let (Some(id), Some(window)) = (
        WATCHDOG.with(|watchdog| *watchdog.borrow()),
        web_sys::window(),
    ) {
        window.clear_timeout_with_handle(id);
    }
    WATCHDOG.with(|watchdog| *watchdog.borrow_mut() = None);
}
