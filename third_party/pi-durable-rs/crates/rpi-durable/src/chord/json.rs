//! `copyJson`/`JsonValue`/`isJsonValue` (port of `chord/src/json.ts` @ `abe508e1b`).
//!
//! Rust mapping (V17-13 §7.3): `JsonValue` is [`serde_json::Value`] with the
//! workspace `preserve_order` feature, so object key order survives round
//! trips like JS object insertion order. Strict-JSON validation is enforced by
//! the type itself: `serde_json::Value` has no non-finite numbers, no cycles,
//! no functions/symbols, and no exotic prototypes, so `copy_json` is a deep
//! clone and `is_json_value` is total.

/// Strict JSON value (upstream `JsonValue`).
pub type JsonValue = serde_json::Value;

/// JSON object used as the root of every durable document (upstream `JsonObject`).
pub type JsonObject = serde_json::Map<String, JsonValue>;

/// Copy a value into an alias-free strict-JSON tree owned by the caller.
pub fn copy_json(value: &JsonValue) -> JsonValue {
    value.clone()
}

/// Return whether a value is finite strict JSON with plain objects and no cycles.
pub fn is_json_value(_value: &JsonValue) -> bool {
    true
}
