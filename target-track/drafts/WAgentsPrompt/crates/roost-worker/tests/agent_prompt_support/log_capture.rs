//! Every `tracing` event emitted on the current thread, rendered as one line of
//! `message field=value …`, so a suite can assert what the worker logged and —
//! the point of it — what it never logged. Thread-local by construction
//! (`set_default`), so the suites that use it run on a current-thread runtime
//! and await the code under test on the test's own thread.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

/// Install the capture; the reader returns every line so far, the guard keeps
/// it installed.
pub fn capture_events() -> (impl Fn() -> Vec<String>, tracing::subscriber::DefaultGuard) {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);
    let guard = tracing::subscriber::set_default(LineCapture(sink));
    let read = move || recorded.lock().expect("the capture lock").clone();
    (read, guard)
}

struct LineCapture(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for LineCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut line = LineFields::default();
        event.record(&mut line);
        let rendered = format!("{} {}", line.message, line.fields);
        self.0.lock().expect("the capture lock").push(rendered);
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
