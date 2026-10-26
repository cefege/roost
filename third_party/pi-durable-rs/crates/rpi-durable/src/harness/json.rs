//! Leaf-wise JSON assignment (port of `packages/durable/src/harness/json.ts`
//! @ `abe508e1b`).
//!
//! Chord records a container assignment as one full set and only emits an append
//! when a string leaf is reassigned with a longer string. Writing a partial whole
//! would therefore store and publish the complete message on every flush. This
//! helper walks the current and candidate values together so leaves are assigned
//! in place and the emitted operations stay minimal (appends, leaf sets).

use crate::chord::json::JsonValue;

/// Assign `value` at `target[key]` leaf by leaf.
///
/// `target` must be a JSON object; a non-object target is left unchanged.
pub fn assign_json(target: &mut JsonValue, key: &str, value: JsonValue) {
    assign_at(target, Key::Name(key), value);
}

/// Assign `value` at `target[index]` leaf by leaf.
///
/// `target` must be a JSON array; a non-array target is left unchanged. The array
/// grows with `null` holes when `index` is past its end.
pub fn assign_json_index(target: &mut JsonValue, index: usize, value: JsonValue) {
    assign_at(target, Key::Index(index), value);
}

enum Key<'a> {
    Name(&'a str),
    Index(usize),
}

fn assign_at(target: &mut JsonValue, key: Key<'_>, value: JsonValue) {
    match (target, key) {
        (JsonValue::Object(map), Key::Name(name)) => {
            let slot = map.entry(name.to_string()).or_insert(JsonValue::Null);
            assign_slot(slot, value);
        }
        (JsonValue::Array(items), Key::Index(index)) => {
            if index >= items.len() {
                items.resize(index + 1, JsonValue::Null);
            }
            assign_slot(&mut items[index], value);
        }
        _ => {}
    }
}

fn assign_slot(current: &mut JsonValue, value: JsonValue) {
    if let (JsonValue::Object(current_map), JsonValue::Object(value_map)) = (&mut *current, &value)
    {
        // Drop keys the candidate no longer has; untouched keys keep their place.
        current_map.retain(|name, _| value_map.contains_key(name));
        for (name, child) in value_map {
            let slot = current_map.entry(name.clone()).or_insert(JsonValue::Null);
            assign_slot(slot, child.clone());
        }
        return;
    }
    if let (JsonValue::Array(current_items), JsonValue::Array(value_items)) =
        (&mut *current, &value)
        && current_items.len() <= value_items.len()
    {
        for (index, child) in value_items.iter().enumerate() {
            if index < current_items.len() {
                assign_slot(&mut current_items[index], child.clone());
            } else {
                current_items.push(child.clone());
            }
        }
        return;
    }
    if *current != value {
        *current = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn leaf_string_is_reassigned_in_place() {
        let mut target = json!({ "content": "hello" });
        assign_json(&mut target, "content", json!("hello world"));
        assert_eq!(target, json!({ "content": "hello world" }));
    }

    #[test]
    fn object_assignment_deletes_absent_and_keeps_order() {
        let mut target = json!({ "text": "a", "gone": 1 });
        assign_json(&mut target, "__root", json!(null));
        let mut value = json!({ "text": "ab" });
        // Recurse through a nested object to exercise the object branch.
        assign_json(&mut value, "text", json!("abc"));
        let mut target = json!({ "text": "a", "gone": 1 });
        // Assigning the whole object via a wrapped slot.
        target = json!({ "s": { "text": "a", "gone": 1 } });
        assign_json(&mut target, "s", json!({ "text": "abc" }));
        assert_eq!(target, json!({ "s": { "text": "abc" } }));
    }

    #[test]
    fn array_assignment_appends_new_items() {
        let mut target = json!({ "items": [1, 2] });
        assign_json(&mut target, "items", json!([1, 3, 4]));
        assert_eq!(target, json!({ "items": [1, 3, 4] }));

        let mut growing = json!({ "items": [1] });
        assign_json(&mut growing, "items", json!([1, 2, 3]));
        assert_eq!(growing, json!({ "items": [1, 2, 3] }));
    }

    #[test]
    fn shorter_array_replaces_whole() {
        let mut target = json!({ "items": [1, 2, 3] });
        assign_json(&mut target, "items", json!([9]));
        assert_eq!(target, json!({ "items": [9] }));
    }

    #[test]
    fn index_assignment_grows_with_holes() {
        let mut target = json!([1, 2]);
        assign_json_index(&mut target, 3, json!(4));
        assert_eq!(target, json!([1, 2, null, 4]));
    }
}
