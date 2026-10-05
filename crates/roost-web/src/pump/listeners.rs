//! A token-keyed registry of `Fn(u64)` callbacks the pump calls after a
//! dispatch drains: one for sweeps (the core's monotonic clock), one for
//! painted-frame movement (the store's `frames_revision`).
//!
//! ONE list per signal, not a second timer or a reactive hop: a host registers
//! a listener (`Pump::on_sweep`, `Pump::on_frames`), `Pump::dispatch` calls
//! it, and a token takes the registration back.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::Pump;

/// One host callback, handed the pump's reading.
type Listener = Rc<dyn Fn(u64)>;

/// The registered listeners, keyed by the token `register` handed back.
#[derive(Default)]
pub(super) struct Listeners {
    entries: RefCell<Vec<(u64, Listener)>>,
    next_token: Cell<u64>,
    /// Set while the listeners run, so a listener that dispatches does not
    /// recurse back into them: the pass in progress already drains whatever
    /// that dispatch queued.
    notifying: Cell<bool>,
}

impl std::fmt::Debug for Listeners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Listeners")
            .field("listeners", &self.entries.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Listeners {
    /// No listeners. A token starts at 1 so `0` stays "nobody".
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Register `listener` and name it.
    pub(super) fn register(&self, listener: Listener) -> u64 {
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

    /// Call every registered listener with `reading`.
    pub(super) fn notify(&self, reading: u64) {
        if self.notifying.replace(true) {
            return;
        }
        let listeners: Vec<Listener> = self
            .entries
            .borrow()
            .iter()
            .map(|(_, listener)| Rc::clone(listener))
            .collect();
        for listener in listeners {
            listener(reading);
        }
        self.notifying.set(false);
    }
}

impl Pump {
    /// Call every registered sweep listener with the core's own clock.
    ///
    /// AFTER the queue drains and `dispatching` is clear, so a listener may
    /// dispatch, write the store and follow a route without re-entering the
    /// core.
    pub(super) fn notify_sweep_listeners(&self) {
        let now_ms = {
            let core = self.inner.core.borrow();
            roost_client_core::Clock::now_ms(core.clock())
        };
        self.inner.sweeps.notify(now_ms);
    }

    /// Call every registered frames listener with the store's frame counter.
    ///
    /// AFTER the queue drains, like the sweep listeners, so a painter that
    /// reads the store borrows it alone.
    pub(super) fn notify_frame_listeners(&self) {
        let frames_revision = self.inner.core.borrow().store().frames_revision();
        self.inner.frames.notify(frames_revision);
    }

    /// Run `listener` once after every dispatch that moved a painted frame.
    /// The returned token is what `remove_frame_listener` takes.
    pub fn on_frames(&self, listener: Rc<dyn Fn(u64)>) -> u64 {
        self.inner.frames.register(listener)
    }

    /// Stop calling the frames listener `token` names.
    pub fn remove_frame_listener(&self, token: u64) {
        self.inner.frames.remove(token);
    }

    /// Run `listener` after every sweep this pump dispatches. The returned
    /// token is what `remove_sweep_listener` takes.
    ///
    /// The listener receives the core's monotonic reading, never a wall clock:
    /// every deadline the sweep evaluates was armed on that timeline.
    pub fn on_sweep(&self, listener: Rc<dyn Fn(u64)>) -> u64 {
        self.inner.sweeps.register(listener)
    }

    /// Stop calling the listener `token` names.
    pub fn remove_sweep_listener(&self, token: u64) {
        self.inner.sweeps.remove(token);
    }
}
