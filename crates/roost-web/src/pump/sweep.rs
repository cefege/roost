//! The shell bridges that read the pump's clock, and the one moment they are
//! called.
//!
//! ONE list, not a second timer. The browser's sweep is already running on
//! `ClientEvent::Sweep`, and a bridge that opened its own interval would be a
//! second answer to "when" — so the registry is the whole mechanism: a host
//! registers a listener, the pump calls it with the core's own monotonic
//! reading, and a token takes the registration back.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// One shell bridge's subscription to the pump's clock.
type SweepListener = Rc<dyn Fn(u64)>;

/// The registered listeners, keyed by the token `register` handed back.
#[derive(Default)]
pub(super) struct SweepListeners {
    entries: RefCell<Vec<(u64, SweepListener)>>,
    next_token: Cell<u64>,
    /// Set while the listeners run, so a listener that dispatches does not
    /// recurse back into them: the pass in progress already drains whatever
    /// that dispatch queued.
    notifying: Cell<bool>,
}

impl std::fmt::Debug for SweepListeners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SweepListeners")
            .field("listeners", &self.entries.borrow().len())
            .finish_non_exhaustive()
    }
}

impl SweepListeners {
    /// No listeners. A token starts at 1 so `0` stays "nobody".
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Register `listener` and name it.
    pub(super) fn register(&self, listener: SweepListener) -> u64 {
        let token = self.next_token.get();
        self.next_token.set(token + 1);
        self.entries.borrow_mut().push((token, listener));
        token
    }

    /// Take `token` back. A token already gone is not an error: a remount may
    /// have taken the slot before the old scope dropped.
    pub(super) fn remove(&self, token: u64) {
        self.entries.borrow_mut().retain(|(held, _)| *held != token);
    }

    /// Call every registered listener with the pump's own reading.
    pub(super) fn notify(&self, now_ms: u64) {
        if self.notifying.replace(true) {
            return;
        }
        let listeners: Vec<SweepListener> = self
            .entries
            .borrow()
            .iter()
            .map(|(_, listener)| Rc::clone(listener))
            .collect();
        for listener in listeners {
            listener(now_ms);
        }
        self.notifying.set(false);
    }
}
