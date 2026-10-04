//! Every `tracing` event emitted on the current thread, rendered as one line of
//! `message field=value …`, so a suite can assert what the worker logged and —
//! the point of it — what it never logged. Per-thread by sink, so the suites
//! that use it run on a current-thread runtime and await the code under test on
//! the test's own thread; one process-global router files each event under the
//! thread that emitted it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ThreadId};

type Sink = Arc<Mutex<Vec<String>>>;
type Sinks = Arc<Mutex<HashMap<ThreadId, Sink>>>;

/// Install the capture for this thread; the reader returns every line so far,
/// the guard keeps it installed.
pub fn capture_events() -> (impl Fn() -> Vec<String>, CaptureGuard) {
    let sinks = installed_router();
    let recorded: Sink = Arc::default();
    let thread = thread::current().id();
    sinks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(thread, Arc::clone(&recorded));
    let guard = CaptureGuard {
        sinks,
        thread,
        recorded: Arc::clone(&recorded),
    };
    let read = move || recorded.lock().expect("the capture lock").clone();
    (read, guard)
}

/// Uninstalls this thread's capture when dropped.
pub struct CaptureGuard {
    sinks: Sinks,
    thread: ThreadId,
    recorded: Sink,
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        let mut sinks = self.sinks.lock().unwrap_or_else(PoisonError::into_inner);
        if sinks
            .get(&self.thread)
            .is_some_and(|sink| Arc::ptr_eq(sink, &self.recorded))
        {
            sinks.remove(&self.thread);
        }
    }
}

/// The router every thread in the process dispatches to, installed by the
/// first capture.
///
/// Never a per-test `set_default`: `tracing` caches each callsite's interest
/// for the whole process the first time any thread reaches it, and while only
/// one dispatcher is registered it asks the REACHING thread's default. A test
/// without a capture that reaches a shared callsite first, beside a test that
/// holds one, caches `never` and that capture silently stays empty.
fn installed_router() -> Sinks {
    if let Some(sinks) = current_router_sinks() {
        return sinks;
    }
    let router = CaptureRouter::default();
    let sinks = Arc::clone(&router.sinks);
    if tracing::subscriber::set_global_default(router).is_ok() {
        // The router is registered before it is published as the global, so a
        // callsite first reached in between was decided against no subscriber.
        tracing::callsite::rebuild_interest_cache();
        return sinks;
    }
    current_router_sinks().expect("the process subscriber is the capture router")
}

fn current_router_sinks() -> Option<Sinks> {
    tracing::dispatcher::get_default(|dispatch| {
        dispatch
            .downcast_ref::<CaptureRouter>()
            .map(|router| Arc::clone(&router.sinks))
    })
}

#[derive(Default)]
struct CaptureRouter {
    sinks: Sinks,
}

impl tracing::Subscriber for CaptureRouter {
    // Unconditional: `register_callsite` caches this answer for every thread,
    // so it cannot depend on whether the asking thread holds a capture.
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let sink = self
            .sinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&thread::current().id())
            .cloned();
        let Some(sink) = sink else {
            return;
        };
        let mut line = LineFields::default();
        event.record(&mut line);
        let rendered = format!("{} {}", line.message, line.fields);
        sink.lock().expect("the capture lock").push(rendered);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

#[derive(Default)]
struct LineFields {
    message: String,
    fields: String,
}

impl tracing::field::Visit for LineFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            let _ = write!(self.fields, "{}={value:?} ", field.name());
        }
    }
}
