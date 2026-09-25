//! The caller's field bag for one log, diag or signal line, plus the sink a
//! host installs to take whole records off the process. A field is serialized
//! once, here, so no emitter can be broken by a value it cannot encode — the
//! observer never propagates a failure into the path it observes.

use serde::Serialize;
use serde_json::{Map, Value};

/// What a value that refuses to serialize becomes. A loss the operator must
/// see, flagged in place rather than raised into the product path.
pub const UNSERIALIZABLE_PREFIX: &str = "[unserializable:";

/// The caller's structured fields. Builder methods consume and return `self`
/// so a call site reads as one object literal: `LogFields::new().set("k", v)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogFields {
    entries: Map<String, Value>,
}

impl LogFields {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one field. A value `serde_json` cannot represent is stored as a
    /// flagged string so the line still ships and still shows the loss.
    pub fn set(mut self, key: &str, value: impl Serialize) -> Self {
        self.put(key, value);
        self
    }

    /// The in-place form of [`LogFields::set`], for a loop of fields.
    pub fn put(&mut self, key: &str, value: impl Serialize) {
        let encoded = serde_json::to_value(value)
            .unwrap_or_else(|error| Value::String(format!("{UNSERIALIZABLE_PREFIX} {error}]")));
        self.entries.insert(key.to_owned(), encoded);
    }

    /// Splice another bag in last, so the second writer wins a collision —
    /// the same precedence the TypeScript object spread gave the caller's
    /// `kv` over the emitter's own fixed keys.
    pub fn absorb(&mut self, other: LogFields) {
        self.entries.extend(other.entries);
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.get(key)
    }

    /// Remove a field, returning what was there. `cooldownKey` is stripped
    /// this way before a signal record ships.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.entries.remove(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bag as one JSON value, for a sink that wants an object.
    pub fn to_value(&self) -> Value {
        Value::Object(self.entries.clone())
    }

    /// The bag as JSON text, for the `fields` event field. Serialization of a
    /// `Map` of `Value`s cannot fail, so the fallback is unreachable.
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(&self.entries).unwrap_or_else(|_| "{}".to_owned())
    }
}

/// A host-installed destination for whole records. The browser front end
/// installs a sink that batches records to the coordinator; coord and worker
/// leave it unset and records go to the log line on stdout/stderr instead.
pub trait RecordSink: Send + Sync {
    fn emit(&self, record: &LogFields);
}

#[cfg(test)]
mod tests {
    use super::{LogFields, UNSERIALIZABLE_PREFIX};
    use serde::ser::Error as _;
    use serde::{Serialize, Serializer};
    use serde_json::json;

    struct Refuses;

    impl Serialize for Refuses {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("cycle"))
        }
    }

    #[test]
    fn fields_serialize_whatever_the_caller_passes() {
        let fields = LogFields::new()
            .set("cols", 80u32)
            .set("sid", "abc")
            .set("nested", json!({ "a": [1, 2] }));
        assert_eq!(fields.get("cols"), Some(&json!(80)));
        assert_eq!(fields.get("sid"), Some(&json!("abc")));
        assert_eq!(fields.get("nested"), Some(&json!({ "a": [1, 2] })));
        assert_eq!(fields.len(), 3);
        assert!(!fields.is_empty());
        assert!(LogFields::new().is_empty());
    }

    #[test]
    fn a_value_that_cannot_serialize_is_flagged_not_dropped() {
        let fields = LogFields::new().set("input_seq", Refuses);
        let value = fields.get("input_seq").expect("the field survives");
        let rendered = value.as_str().expect("the loss is a string");
        assert!(rendered.starts_with(UNSERIALIZABLE_PREFIX), "{rendered}");
        assert!(rendered.contains("cycle"), "{rendered}");
    }

    #[test]
    fn absorb_lets_the_second_writer_win_a_collision() {
        let mut fields = LogFields::new().set("evt", "fixed").set("keep", 1);
        fields.absorb(LogFields::new().set("evt", "caller"));
        assert_eq!(fields.get("evt"), Some(&json!("caller")));
        assert_eq!(fields.get("keep"), Some(&json!(1)));
    }

    #[test]
    fn remove_takes_the_field_out_of_the_bag() {
        let mut fields = LogFields::new().set("cooldownKey", "sid-1");
        assert_eq!(fields.remove("cooldownKey"), Some(json!("sid-1")));
        assert_eq!(fields.remove("cooldownKey"), None);
        assert!(fields.is_empty());
    }

    #[test]
    fn to_json_is_a_json_object_that_round_trips() {
        let fields = LogFields::new().set("msg", "it's fine").set("n", 3);
        let parsed: serde_json::Value =
            serde_json::from_str(&fields.to_json()).expect("valid JSON");
        assert_eq!(parsed, json!({ "msg": "it's fine", "n": 3 }));
        assert_eq!(fields.to_value(), parsed);
    }
}
