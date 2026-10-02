//! The awaitable queue an attachment carrier's browser callbacks fill, and the
//! deadline a wait on it may end at.
//!
//! Owned by `platform::attachments`; the loopback socket and the peer push into
//! it from their `Closure`s and the carrier's async methods await it. A browser
//! callback never runs carrier logic itself, because it fires on a task the
//! upload's future does not own.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Poll, Waker};

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

/// The task currently waiting, shared by an inbox and the deadline it races.
type WakerSlot = Rc<RefCell<Option<Waker>>>;

/// Events observed by browser callbacks, waiting for the carrier to read them.
#[derive(Debug)]
pub struct EventInbox<T> {
    queue: Rc<RefCell<VecDeque<T>>>,
    waker: WakerSlot,
}

impl<T> Clone for EventInbox<T> {
    fn clone(&self) -> Self {
        Self {
            queue: Rc::clone(&self.queue),
            waker: Rc::clone(&self.waker),
        }
    }
}

impl<T> Default for EventInbox<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> EventInbox<T> {
    /// An empty inbox.
    pub fn new() -> Self {
        Self {
            queue: Rc::new(RefCell::new(VecDeque::new())),
            waker: Rc::new(RefCell::new(None)),
        }
    }

    /// Queue one observation and wake whoever is waiting for it.
    pub fn push(&self, event: T) {
        self.queue.borrow_mut().push_back(event);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }

    /// The next observation, or `None` once `deadline` has elapsed.
    ///
    /// A queued event wins over an elapsed deadline, because the event arrived
    /// first and v2's callbacks would have settled the waiter with it.
    pub async fn next(&self, deadline: &Deadline) -> Option<T> {
        poll_fn(|context| {
            if let Some(event) = self.queue.borrow_mut().pop_front() {
                return Poll::Ready(Some(event));
            }
            if deadline.fired.get() {
                return Poll::Ready(None);
            }
            *self.waker.borrow_mut() = Some(context.waker().clone());
            *deadline.waker.borrow_mut() = Some(context.waker().clone());
            Poll::Pending
        })
        .await
    }
}

/// A browser timer one wait may end at. Dropping it clears the timer, so a
/// settled wait leaves nothing to fire into a carrier that moved on.
#[derive(Debug)]
pub struct Deadline {
    fired: Rc<Cell<bool>>,
    waker: WakerSlot,
    timer: Option<i32>,
    _on_elapsed: Option<Closure<dyn FnMut()>>,
}

impl Deadline {
    /// A deadline `delay_ms` from now.
    pub fn after(delay_ms: u64) -> Self {
        let fired = Rc::new(Cell::new(false));
        let waker: WakerSlot = Rc::new(RefCell::new(None));
        let on_elapsed = {
            let fired = Rc::clone(&fired);
            let waker = Rc::clone(&waker);
            Closure::wrap(Box::new(move || {
                fired.set(true);
                if let Some(waker) = waker.borrow_mut().take() {
                    waker.wake();
                }
            }) as Box<dyn FnMut()>)
        };
        let delay = i32::try_from(delay_ms).unwrap_or(i32::MAX);
        let timer = web_sys::window().and_then(|window| {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    on_elapsed.as_ref().unchecked_ref(),
                    delay,
                )
                .ok()
        });
        // A document with no timer has no deadline to keep, and a wait that
        // can never end is a stuck upload; elapsing at once fails it instead.
        if timer.is_none() {
            fired.set(true);
        }
        Self {
            fired,
            waker,
            timer,
            _on_elapsed: Some(on_elapsed),
        }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        if let (Some(timer), Some(window)) = (self.timer, web_sys::window()) {
            window.clear_timeout_with_handle(timer);
        }
    }
}
