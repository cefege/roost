//! The one place a log line becomes text: one JSON object per line, shaped
//! `{ts, level, target, msg, ...fields}`. The caller's fields are spliced in
//! last, so a caller that names a field `msg`, `ts`, `level` or `target` wins
//! over the fixed key — the same precedence the TypeScript object spread gave
//! `kv` over the emitter's own keys, and the reason `roost doctor` can trust
//! the four fixed keys it reads.

use std::sync::Arc;

use serde_json::{Map, Value};
use tracing::Event;
use tracing::field::{Field, Visit};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};

use crate::clock::EventClock;

/// The `tracing` field the message arrives in.
pub(crate) const MESSAGE_FIELD: &str = "msg";
/// The `tracing` field the caller's namespace arrives in. A `tracing` callsite
/// target has to be a constant, so the namespace the caller names is a field.
pub(crate) const TARGET_FIELD: &str = "target";
/// The `tracing` field the caller's field bag arrives in, as JSON text.
pub(crate) const FIELDS_FIELD: &str = "fields";

/// The formatter every `fmt::layer()` in `init` writes events through.
#[derive(Debug, Clone)]
pub(crate) struct JsonLineFormat {
    clock: Arc<dyn EventClock>,
}

impl JsonLineFormat {
    pub(crate) fn new(clock: Arc<dyn EventClock>) -> Self {
        Self { clock }
    }
}

/// What one event carried: the namespace, the message, any foreign scalar
/// fields, and the caller's own bag.
#[derive(Debug, Default)]
pub(crate) struct EventRecord {
    pub(crate) target: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) foreign: Map<String, Value>,
    pub(crate) fields: Option<Map<String, Value>>,
}

impl EventRecord {
    /// A foreign field whose value is not a JSON scalar is rendered as its
    /// `Debug` text rather than dropped, so an event from a dependency still
    /// carries everything it said.
    fn record_text(&mut self, field: &Field, text: String) {
        match field.name() {
            TARGET_FIELD => self.target = Some(text),
            MESSAGE_FIELD => self.message = Some(text),
            FIELDS_FIELD => {
                self.fields = serde_json::from_str::<Map<String, Value>>(&text).ok();
            }
            _ => {
                self.foreign
                    .insert(field.name().to_owned(), Value::String(text));
            }
        }
    }
}

impl Visit for EventRecord {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_text(field, value.to_owned());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_text(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_text(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record_text(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_text(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_text(field, format!("{value:?}"));
    }
}

/// Render one line. Pure: the clock arrives as an argument, so the shape is
/// testable without a subscriber and without a wall clock. An event that
/// carries no namespace of its own — one from outside this facade — is
/// reported under the `tracing` target it was raised with.
pub(crate) fn render_line(
    clock: &dyn EventClock,
    level: tracing::Level,
    callsite_target: &str,
    mut record: EventRecord,
) -> String {
    let mut line = Map::new();
    line.insert("ts".to_owned(), Value::from(clock.now_epoch_ms()));
    line.insert("level".to_owned(), Value::from(level_name(level)));
    let namespace = record
        .target
        .take()
        .unwrap_or_else(|| callsite_target.to_owned());
    line.insert(TARGET_FIELD.to_owned(), Value::from(namespace));
    if let Some(message) = record.message.take() {
        line.insert(MESSAGE_FIELD.to_owned(), Value::from(message));
    }
    line.extend(record.foreign);
    if let Some(fields) = record.fields.take() {
        line.extend(fields);
    }
    Value::Object(line).to_string()
}

fn level_name(level: tracing::Level) -> &'static str {
    match level {
        tracing::Level::ERROR => "error",
        tracing::Level::WARN => "warn",
        tracing::Level::INFO => "info",
        tracing::Level::DEBUG => "debug",
        // The four-level gate never lets a trace event reach a layer; if one
        // arrives from outside the facade it is still reported, not dropped.
        tracing::Level::TRACE => "trace",
    }
}

impl<S, N> FormatEvent<S, N> for JsonLineFormat
where
    S: tracing::Subscriber
        + for<'lookup_span> tracing_subscriber::registry::LookupSpan<'lookup_span>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let mut record = EventRecord::default();
        event.record(&mut record);
        let line = render_line(
            self.clock.as_ref(),
            *event.metadata().level(),
            event.metadata().target(),
            record,
        );
        writeln!(writer, "{line}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{EventRecord, render_line};
    use crate::clock::FixedClock;

    fn record(
        target: Option<&str>,
        message: Option<&str>,
        fields: Option<serde_json::Value>,
    ) -> EventRecord {
        EventRecord {
            target: target.map(str::to_owned),
            message: message.map(str::to_owned),
            foreign: serde_json::Map::new(),
            fields: fields.map(|value| value.as_object().expect("an object").clone()),
        }
    }

    #[test]
    fn a_line_carries_the_four_fixed_keys_and_the_caller_fields() {
        let clock = FixedClock::new(1_700_000_000_123, 7);
        let line = render_line(
            &clock,
            tracing::Level::INFO,
            "roost_observability::log",
            record(
                Some("coord.db"),
                Some("event.appended"),
                Some(json!({ "rows": 3, "sid": "abc" })),
            ),
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).expect("one JSON object"),
            json!({
                "ts": 1_700_000_000_123_i64,
                "level": "info",
                "target": "coord.db",
                "msg": "event.appended",
                "rows": 3,
                "sid": "abc",
            })
        );
    }

    #[test]
    fn a_caller_field_wins_over_the_fixed_key_it_collides_with() {
        let clock = FixedClock::new(1_700_000_000_000, 0);
        let line = render_line(
            &clock,
            tracing::Level::WARN,
            "roost_observability::log",
            record(
                Some("signal"),
                Some("keeper.died"),
                Some(json!({
                    "msg": "caller wins",
                    "target": "caller.target",
                    "level": "debug",
                    "ts": 5,
                })),
            ),
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
        assert_eq!(parsed["msg"], json!("caller wins"));
        assert_eq!(parsed["target"], json!("caller.target"));
        assert_eq!(parsed["level"], json!("debug"));
        assert_eq!(parsed["ts"], json!(5));
    }

    #[test]
    fn an_event_with_no_namespace_of_its_own_keeps_its_callsite_target() {
        let clock = FixedClock::new(1, 0);
        let line = render_line(
            &clock,
            tracing::Level::ERROR,
            "some_dependency",
            record(None, Some("boom"), None),
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
        assert_eq!(parsed["target"], json!("some_dependency"));
        assert_eq!(parsed["msg"], json!("boom"));
    }

    #[test]
    fn every_level_renders_its_own_lowercase_name() {
        let clock = FixedClock::new(1, 0);
        for (level, name) in [
            (tracing::Level::DEBUG, "debug"),
            (tracing::Level::INFO, "info"),
            (tracing::Level::WARN, "warn"),
            (tracing::Level::ERROR, "error"),
            (tracing::Level::TRACE, "trace"),
        ] {
            let line = render_line(
                &clock,
                level,
                "roost_observability::log",
                EventRecord::default(),
            );
            let parsed: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
            assert_eq!(parsed["level"], json!(name));
        }
    }
}
