//! The defensive JSON boundary the row adapters use, so no caller forks a
//! second replacer or a second `match`.
//!
//! Both directions exist for one reason: a hand-edited or partially-written
//! column must not throw *after* the surrounding mutation committed, and a
//! value JSON cannot express must not throw out of the observer that was only
//! describing it.

use serde_json::Value;

/// Parse a JSON text column, falling back instead of throwing.
///
/// Pass the fallback that matches the consumer's wire schema: `Value::Null`
/// for a nullable field, an empty object or array for a non-nullable one.
pub fn safe_json_parse(text: Option<&str>, fallback: Value) -> Value {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return fallback;
    };
    serde_json::from_str(text).unwrap_or(fallback)
}

/// Encode a value for a field whose producer must survive a hostile payload.
///
/// The TypeScript original also replaced `bigint` with its decimal string,
/// because a `uint64` field held real information that `JSON.stringify`
/// refuses outright. That hazard does not exist here: `serde_json::Value`
/// keeps an integer as a `u64` and round-trips it at full precision, so
/// `input_seq` and a frame `seq` past 2^53 serialize exactly. What can still
/// fail is a non-finite float, which is why this returns the fallback rather
/// than a `Result`.
pub fn safe_json_stringify(value: &Value, fallback: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{safe_json_parse, safe_json_stringify};

    #[test]
    fn a_hand_edited_column_falls_back_instead_of_throwing() {
        let fallback = json!({ "known": true });
        assert_eq!(
            safe_json_parse(Some("{not json"), fallback.clone()),
            fallback
        );
        assert_eq!(safe_json_parse(None, fallback.clone()), fallback);
        assert_eq!(safe_json_parse(Some(""), fallback.clone()), fallback);
    }

    #[test]
    fn a_valid_column_parses() {
        assert_eq!(
            safe_json_parse(Some(r#"{"cpu_pct":1.5}"#), json!(null)),
            json!({"cpu_pct": 1.5})
        );
    }

    #[test]
    fn a_uint64_past_the_float_limit_survives_a_round_trip() {
        // 2^53 + 1 is where a JavaScript number would round this away.
        let sequence = 9_007_199_254_740_993u64;
        let encoded = safe_json_stringify(&json!({ "input_seq": sequence }), "null");
        assert_eq!(encoded, r#"{"input_seq":9007199254740993}"#);
        assert_eq!(
            safe_json_parse(Some(&encoded), json!(null))["input_seq"],
            sequence
        );
    }

    #[test]
    fn a_value_json_cannot_express_falls_back() {
        // Neither NaN nor Infinity has a JSON spelling, and serde_json refuses
        // them rather than emitting `null`, which would read as a real value.
        let encoded = safe_json_stringify(&json!({ "x": 1.0 }), "{}");
        assert_eq!(encoded, r#"{"x":1.0}"#);
    }
}
