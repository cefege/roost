//! The finalize watchdog: the one deadline the mic itself owns.
//!
//! Split out of `super::input` because a timer outlives the call
//! that armed it and outlives the recording it was watching, so it is a
//! lifetime of its own rather than a line of the component's body. The engine
//! has its own, shorter wait; this is the outer bound, so a wedged engine cannot
//! leave the composer saying `finalizing` forever. Both the arm and the cancel
//! live here so a component that stops being mounted has one door to call.
//! One `ComposerContext` owns one watchdog; dropping it cancels its timer.

use std::cell::Cell;
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

/// How long a finalize waits for the engine before the composer gives up on it.
pub const FINALIZE_WATCHDOG_MS: i32 = 6_000;

/// The finalize watchdog's timer, held so the composer can cancel it.
#[derive(Debug, Default)]
pub struct FinalizeWatchdog {
    timer: Rc<Cell<Option<i32>>>,
}

impl FinalizeWatchdog {
    /// Wait this long for a final result, then run `on_expiry`, which finishes
    /// the recording anyway. Re-arming cancels the previous deadline.
    pub fn arm(&self, on_expiry: impl FnOnce() + 'static) {
        self.clear();
        #[cfg(target_arch = "wasm32")]
        if let Some(window) = web_sys::window() {
            let timer = Rc::clone(&self.timer);
            let expiry = wasm_bindgen::closure::Closure::once(move || {
                // Forgotten before the finish runs, so the finish's own cancel
                // does not clear a timer that has already fired.
                timer.set(None);
                on_expiry();
            });
            if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                expiry.as_ref().unchecked_ref(),
                FINALIZE_WATCHDOG_MS,
            ) {
                expiry.forget();
                self.timer.set(Some(id));
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        drop(on_expiry);
    }

    /// Cancel the finalize watchdog.
    pub fn clear(&self) {
        #[cfg(target_arch = "wasm32")]
        if let (Some(id), Some(window)) = (self.timer.get(), web_sys::window()) {
            window.clear_timeout_with_handle(id);
        }
        self.timer.set(None);
    }
}

impl Drop for FinalizeWatchdog {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cleared_watchdog_does_not_fire() {
        let watchdog = FinalizeWatchdog::default();
        watchdog.timer.set(Some(7));
        watchdog.clear();
        assert_eq!(watchdog.timer.get(), None);
    }
}
