//! The operational log facade. One JSON line per event, four fixed keys, and
//! the caller's namespace and fields on top. `warn` and `error` reach stderr
//! (the `*.err.log` channel `roost doctor` reads), `debug` and `info` reach
//! stdout; the split and the minimum level are the subscriber's business, in
//! [`crate::init`], so nothing here reads the environment or a clock.

use crate::fields::LogFields;

/// One event, three fields.
///
/// The level is passed as a constant because `tracing` builds each callsite's
/// metadata in a `static`, and a `static` cannot hold a value the caller
/// computed — a level that arrived as a function argument would not compile.
/// The field names are identifiers because `tracing` stringifies the token it
/// is handed, so a constant in that position records the constant's own name;
/// `crate::line` matches on these same three names. The namespace is a field
/// rather than the callsite target for the same reason, and because the
/// namespace is the caller's, not this module path.
macro_rules! emit {
    ($level:expr, $target:expr, $message:expr, $fields_json:expr) => {{
        // Bound to locals first: `tracing`'s `%` sigil has to lex an
        // expression, and a metavariable substituted into that position is an
        // opaque fragment it cannot re-parse. The names are what
        // `crate::line` matches on, so the locals must carry them.
        let target = $target;
        let message = $message;
        let fields_json = $fields_json;
        tracing::event!(
            $level,
            msg = %message,
            target = %target,
            fields = %fields_json,
        );
    }};
}

/// Record a `debug` event. Suppressed unless `ROOST_LOG_LEVEL=debug`; a
/// per-keystroke or per-token callsite belongs here, not on an always-on path.
pub fn debug(target: &str, message: &str, fields: LogFields) {
    emit!(tracing::Level::DEBUG, target, message, fields.to_json());
}

/// Record an `info` event: one state transition that matters.
pub fn info(target: &str, message: &str, fields: LogFields) {
    emit!(tracing::Level::INFO, target, message, fields.to_json());
}

/// Record a `warn` event on the stderr channel.
pub fn warn(target: &str, message: &str, fields: LogFields) {
    emit!(tracing::Level::WARN, target, message, fields.to_json());
}

/// Record an `error` event on the stderr channel.
pub fn error(target: &str, message: &str, fields: LogFields) {
    emit!(tracing::Level::ERROR, target, message, fields.to_json());
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::layer::{Context, Layer};

    use super::{debug, error, info, warn};
    use crate::fields::LogFields;
    use crate::line::EventRecord;

    type Captured = (String, Option<String>, Option<String>, Option<Value>);

    /// A thread-local recording subscriber, so a test can read what an event
    /// carried without a global subscriber or a captured stream.
    #[derive(Clone, Default)]
    struct Capture {
        events: Arc<Mutex<Vec<Captured>>>,
    }

    impl<S: Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            let mut record = EventRecord::default();
            event.record(&mut record);
            let mut events = match self.events.lock() {
                Ok(events) => events,
                // A poisoned lock means another test panicked; skip rather
                // than cascade that failure into an unrelated assertion.
                Err(_) => return,
            };
            events.push((
                event.metadata().level().to_string(),
                record.target,
                record.message,
                record.fields.map(Value::Object),
            ));
        }
    }

    fn captured_by(capture: &Capture, emit_events: impl FnOnce()) -> Vec<Captured> {
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::TRACE)
            .with(capture.clone());
        // `tracing` decides once per callsite whether anything is interested
        // and caches that answer process-wide, so a test running beside this
        // one can leave a callsite marked un-interesting and this capture would
        // silently see three events instead of four. Rebuilding the cache under
        // this subscriber is the documented way out, and it is what makes the
        // test independent of the order the suite happens to run in.
        tracing::callsite::rebuild_interest_cache();
        tracing::subscriber::with_default(subscriber, emit_events);
        tracing::callsite::rebuild_interest_cache();
        let events = match capture.events.lock() {
            Ok(events) => events,
            Err(_) => return Vec::new(),
        };
        events.clone()
    }

    #[test]
    fn each_level_carries_its_namespace_message_and_field_bag() {
        let capture = Capture::default();
        let events = captured_by(&capture, || {
            debug("coord.db", "spun", LogFields::new().set("pid", 7));
            info(
                "coord.db",
                "event.appended",
                LogFields::new().set("rows", 3),
            );
            warn("signal", "keeper.died", LogFields::new().set("sid", "abc"));
            error("coord.db", "append failed", LogFields::new());
        });
        let levels: Vec<&str> = events.iter().map(|(level, ..)| level.as_str()).collect();
        assert_eq!(
            levels,
            ["DEBUG", "INFO", "WARN", "ERROR"],
            "every level reached the subscriber"
        );
        assert_eq!(events[0].0, "DEBUG");
        assert_eq!(events[1].0, "INFO");
        assert_eq!(events[2].0, "WARN");
        assert_eq!(events[3].0, "ERROR");
        assert_eq!(events[1].1.as_deref(), Some("coord.db"));
        assert_eq!(events[1].2.as_deref(), Some("event.appended"));
        assert_eq!(events[1].3, Some(serde_json::json!({ "rows": 3 })));
        assert_eq!(events[2].3, Some(serde_json::json!({ "sid": "abc" })));
        assert_eq!(events[3].3, Some(serde_json::json!({})));
    }

    #[test]
    fn the_namespace_is_the_callers_own_name_not_the_module_path() {
        let capture = Capture::default();
        let events = captured_by(&capture, || info("coord.sync", "resumed", LogFields::new()));
        assert_eq!(events[0].1.as_deref(), Some("coord.sync"));
    }
}
