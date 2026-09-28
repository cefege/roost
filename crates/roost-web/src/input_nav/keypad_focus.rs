//! Focus the terminal key pad's first key for a device with no pointer:
//! opening the pad is useless to a controller until focus is inside it. The
//! sheet mounts from a store write, so the grid is absent for at least one
//! turn — retry by frame, bounded, and stop as soon as a key actually took
//! focus. The retry rule is [`FirstKeyFocusRetry`]; the frame loop is wasm.
//! Called by `pad_dom::BrowserPadDom::start_keypad_focus`.
//! Ported from `apps/web/src/store/terminalNavPad.ts` (`focusTerminalNavPadFirstKey`).

/// The key pad's first enabled key.
pub const KEYPAD_FIRST_KEY_SELECTOR: &str = ".term-nav__grid button:not(:disabled)";

/// Attempts before giving up: one on the next microtask, then one per frame.
pub const FIRST_KEY_FOCUS_ATTEMPTS: u32 = 4;

/// What to do after one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryStep {
    /// Focus took, or the retry was cancelled, or its budget ran out.
    Stop,
    /// Try again on the next animation frame.
    NextFrame,
}

/// The bounded, cancellable retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FirstKeyFocusRetry {
    attempts: u32,
    cancelled: bool,
}

impl FirstKeyFocusRetry {
    /// No attempt yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stand the retry down; a pending frame must not focus anything.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Whether an attempt may run now.
    pub fn may_attempt(&self) -> bool {
        !self.cancelled && self.attempts < FIRST_KEY_FOCUS_ATTEMPTS
    }

    /// Record one attempt: `focus_took` when a key exists and is now
    /// `document.activeElement`.
    pub fn record_attempt(&mut self, focus_took: bool) -> RetryStep {
        if focus_took || self.cancelled {
            return RetryStep::Stop;
        }
        self.attempts += 1;
        if self.attempts < FIRST_KEY_FOCUS_ATTEMPTS { RetryStep::NextFrame } else { RetryStep::Stop }
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) use browser::start_first_key_focus;

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::RefCell;
    use std::rc::Rc;

    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    use super::{FirstKeyFocusRetry, KEYPAD_FIRST_KEY_SELECTOR, RetryStep};
    use crate::input_nav::dom_read;
    use crate::input_nav::pad_surfaces::KeypadFocusCancel;

    #[derive(Default)]
    struct RetryLoop {
        retry: FirstKeyFocusRetry,
        frame: Option<i32>,
        callback: Option<Closure<dyn FnMut()>>,
    }

    /// Start the retry; the returned closure cancels it.
    pub(crate) fn start_first_key_focus() -> KeypadFocusCancel {
        let state = Rc::new(RefCell::new(RetryLoop::default()));
        let weak = Rc::downgrade(&state);
        let frame_weak = weak.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            if let Some(state) = frame_weak.upgrade() {
                attempt(&state);
            }
        });
        state.borrow_mut().callback = Some(callback);
        // The first attempt runs on the microtask queue, after the store write
        // that opened the pad has had its turn to schedule the sheet.
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(state) = weak.upgrade() {
                attempt(&state);
            }
        });
        Box::new(move || {
            let mut state = state.borrow_mut();
            state.retry.cancel();
            if let (Some(frame), Some(window)) = (state.frame.take(), web_sys::window()) {
                let _ = window.cancel_animation_frame(frame);
            }
            state.callback = None;
        })
    }

    fn attempt(state: &Rc<RefCell<RetryLoop>>) {
        {
            let mut guard = state.borrow_mut();
            guard.frame = None;
            if !guard.retry.may_attempt() {
                return;
            }
        }
        // No borrow across `focus()`: it fires focus handlers synchronously.
        let target = dom_read::document()
            .and_then(|document| document.query_selector(KEYPAD_FIRST_KEY_SELECTOR).ok().flatten());
        let focus_took = target.is_some_and(|target| {
            if let Some(html) = dom_read::html(&target) {
                let _ = html.focus();
            }
            dom_read::active_element().is_some_and(|active| active == target)
        });
        let mut guard = state.borrow_mut();
        match guard.retry.record_attempt(focus_took) {
            RetryStep::Stop => {
                tracing::debug!(target: "input_nav", focus_took, "keypad first-key focus settled");
            }
            RetryStep::NextFrame => {
                let frame = match (web_sys::window(), guard.callback.as_ref()) {
                    (Some(window), Some(callback)) => {
                        window.request_animation_frame(callback.as_ref().unchecked_ref()).ok()
                    }
                    _ => None,
                };
                guard.frame = frame;
            }
        }
    }
}
