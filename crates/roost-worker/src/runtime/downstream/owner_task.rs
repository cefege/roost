//! How a downstream owner's work runs beside the link loop: the synchronous
//! half now, in receive order, the future on its own task, and exactly ONE
//! completion either way — a panic in either half, or a task dropped before it
//! answered, completes too. Called by `runtime::downstream::terminal`. Ports the
//! `void (async () => deps.onX(..))().catch(..).finally(..)` shape of v2
//! `apps/worker/src/transport/coord-link-downstream.ts`.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::FutureExt as _;

use crate::uplink::OwnerFuture;

/// What a request is answered with when its owner's task was dropped before
/// the owner answered (a runtime shutting down under it).
pub(super) const OWNER_CANCELLED: &str = "the worker owner was cancelled before it answered";

/// What happens with an owner's outcome; `Err` carries the failure message.
pub(super) type Completion<T> = Box<dyn FnOnce(Result<T, String>) + Send + 'static>;

/// Completed explicitly, or on drop as cancelled — never twice, never not.
struct PendingCompletion<T> {
    finish: Option<Completion<T>>,
}

impl<T> PendingCompletion<T> {
    fn complete(mut self, outcome: Result<T, String>) {
        if let Some(finish) = self.finish.take() {
            finish(outcome);
        }
    }
}

impl<T> Drop for PendingCompletion<T> {
    fn drop(&mut self) {
        if let Some(finish) = self.finish.take() {
            tracing::warn!("a downstream owner's task ended without an answer");
            finish(Err(OWNER_CANCELLED.to_owned()));
        }
    }
}

/// Call `start` NOW — so an owner's synchronous reservation happens before the
/// next frame is read, which is v2's "invoke synchronously" rule — then await
/// its future on a task and hand the outcome to `finish`.
pub(super) fn run_owner<T: Send + 'static>(
    start: impl FnOnce() -> OwnerFuture<T>,
    finish: Completion<T>,
) {
    let pending = PendingCompletion {
        finish: Some(finish),
    };
    let future = match std::panic::catch_unwind(AssertUnwindSafe(start)) {
        Ok(future) => future,
        Err(payload) => {
            pending.complete(Err(panic_message(payload.as_ref())));
            return;
        }
    };
    tokio::spawn(async move {
        let outcome = AssertUnwindSafe(future)
            .catch_unwind()
            .await
            .map_err(|payload| panic_message(payload.as_ref()));
        pending.complete(outcome);
    });
}

/// A panic's own message where it has one, as v2 answers with `error.message`.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "the worker owner panicked".to_owned()
}

/// One admitted request against a bounded in-flight count. Dropping it is v2's
/// `.finally(() => inFlight -= 1)`, so a panic or a dropped task frees it too.
#[derive(Debug)]
pub(super) struct InFlightSlot {
    counter: Arc<AtomicUsize>,
}

impl InFlightSlot {
    /// A slot, or `None` when `cap` requests are already in flight.
    pub(super) fn try_take(counter: &Arc<AtomicUsize>, cap: usize) -> Option<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |in_flight| {
                (in_flight < cap).then_some(in_flight + 1)
            })
            .ok()
            .map(|_| Self {
                counter: Arc::clone(counter),
            })
    }
}

impl Drop for InFlightSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}
