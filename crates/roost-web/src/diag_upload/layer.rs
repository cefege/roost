//! Converting selected tracing events into coordinator diagnostic entries.
//!
//! The layer is filtered independently from console output: carrier decisions
//! and warnings/errors are always retained, while ordinary info stays local.
//! Nothing emitted inside the upload's own `diag_send` span is captured.

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use super::queue::DiagUploadQueue;
use roost_client_core::client::rpc::calls::diag_debug_log_batch::DiagDebugLogEntry;

/// The target of the span every upload call runs in. A warning the upload
/// itself causes — an unauthenticated dispatch, a refused call — would
/// otherwise be queued for the next upload, which fails the same way: a
/// signed-out tab would fill its queue with its own failures and evict the
/// evidence it holds.
pub(crate) const SEND_SPAN_TARGET: &str = "diag_send";

pub(crate) fn filter() -> Targets {
    Targets::new()
        .with_default(LevelFilter::WARN)
        .with_target("carriers", LevelFilter::INFO)
        .with_target("diag_upload", LevelFilter::OFF)
        .with_target(SEND_SPAN_TARGET, LevelFilter::TRACE)
}

pub(crate) struct DiagUploadLayer {
    queue: DiagUploadQueue,
}

impl DiagUploadLayer {
    pub(crate) fn new(queue: DiagUploadQueue) -> Self {
        Self { queue }
    }
}

impl<S> Layer<S> for DiagUploadLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        let inside_upload = context.event_scope(event).is_some_and(|mut scope| {
            scope.any(|span| span.metadata().target() == SEND_SPAN_TARGET)
        });
        if inside_upload {
            return;
        }
        let metadata = event.metadata();
        let mut visitor = EventFields::default();
        event.record(&mut visitor);
        let evt = visitor
            .message
            .take()
            .unwrap_or_else(|| metadata.name().to_owned());
        let mut fields = visitor.fields;
        fields.insert(
            "target".to_owned(),
            Value::String(metadata.target().to_owned()),
        );
        fields.insert(
            "level".to_owned(),
            Value::String(metadata.level().as_str().to_ascii_lowercase()),
        );
        let sid = fields
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.queue.push(DiagDebugLogEntry {
            evt,
            ts_ms: wall_time_ms(),
            mono_ns: monotonic_time_ns(),
            sid,
            kv_json: Value::Object(fields).to_string(),
            signal: true,
            ..Default::default()
        });
    }
}

#[derive(Default)]
struct EventFields {
    fields: Map<String, Value>,
    message: Option<String>,
}

impl EventFields {
    fn insert(&mut self, field: &Field, value: Value) {
        if field.name() == "message" {
            self.message = Some(
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            );
        }
        self.fields.insert(field.name().to_owned(), value);
    }
}

impl Visit for EventFields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::String(value.to_owned()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.insert(field, Value::String(format!("{value:?}")));
    }
}

#[cfg(target_arch = "wasm32")]
fn wall_time_ms() -> u64 {
    js_sys::Date::now() as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn wall_time_ms() -> u64 {
    0
}

#[cfg(target_arch = "wasm32")]
fn monotonic_time_ns() -> u64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| (performance.now() * 1_000_000.0) as u64)
        .unwrap_or(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn monotonic_time_ns() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn layer_serializes_fields_and_filters_by_target_and_level() {
        let queue = DiagUploadQueue::default();
        let subscriber = tracing_subscriber::registry()
            .with(DiagUploadLayer::new(queue.clone()).with_filter(filter()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "carriers", session_id = "sid-1", worker = "worker-a", attempt = 7_u64, ready = true, display = ?Some("value"), "direct route promoted");
            tracing::info!(target: "other", "not uploaded");
            tracing::warn!(target: "other", session_id = "sid-2", "warning uploaded");
        });
        let entries = queue.drain(8);
        assert_eq!(entries.len(), 2);
        let event: Value = serde_json::from_str(&entries[0].kv_json).expect("valid event JSON");
        assert_eq!(entries[0].evt, "direct route promoted");
        assert_eq!(entries[0].sid, "sid-1");
        assert!(entries[0].signal);
        assert_eq!(event["target"], "carriers");
        assert_eq!(event["level"], "info");
        assert_eq!(event["worker"], "worker-a");
        assert_eq!(event["attempt"], 7);
        assert_eq!(event["ready"], true);
        assert_eq!(event["display"], "Some(\"value\")");
        let warning: Value = serde_json::from_str(&entries[1].kv_json).expect("valid event JSON");
        assert_eq!(entries[1].evt, "warning uploaded");
        assert_eq!(entries[1].sid, "sid-2");
        assert_eq!(warning["level"], "warn");
        assert_eq!(warning["target"], "other");
    }

    #[test]
    fn an_event_inside_the_upload_span_is_not_queued() {
        let queue = DiagUploadQueue::default();
        let subscriber = tracing_subscriber::registry()
            .with(DiagUploadLayer::new(queue.clone()).with_filter(filter()));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(target: SEND_SPAN_TARGET, "diagnostic upload");
            span.in_scope(|| {
                tracing::warn!(target: "rpc", "device credential unavailable");
            });
            tracing::warn!(target: "rpc", "outside the upload");
        });
        let entries = queue.drain(8);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].evt, "outside the upload");
    }
}
