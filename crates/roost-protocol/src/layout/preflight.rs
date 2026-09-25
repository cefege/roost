//! The iterative resource preflight for a layout document. It runs before the
//! recursive parse, and it runs iteratively so a deep document cannot exhaust
//! the stack.
//!
//! This pass bounds WORK; it decides nothing about shape. A value that is not a
//! layout document at all, or a field that is not a string, passes through
//! untouched and is judged by `document`'s parse. A value that is a layout
//! document and is too large is refused here, before any tree is built.

use serde_json::{Map, Value};

use crate::ProtocolResult;
use crate::error::ProtocolError;

pub const LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES: usize = 256;
pub const LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES: usize = 256;
/// The root node is depth 1; every split child advances one level.
pub const LAYOUT_DOCUMENT_MAX_DEPTH: usize = 32;
pub const LAYOUT_DOCUMENT_MAX_NODES: usize = 255;
pub const LAYOUT_DOCUMENT_MAX_SLOTS: usize = 512;
pub const LAYOUT_DOCUMENT_MAX_BINDINGS: usize = 512;

const LAYOUT_KEY_LABEL: &str = "layout key";
const LAYOUT_SESSION_ID_LABEL: &str = "layout session_id";

struct PendingLayoutNode<'a> {
    node: &'a Value,
    path: String,
    depth: usize,
}

/// Refuse a document that is too large to parse, and pass everything else on.
///
/// The walk is a worklist, not a recursion, because the whole point of this
/// pass is to bound a hostile document before any tree is built for it.
pub fn preflight_layout_document_resources(input: &Value) -> ProtocolResult<()> {
    let Some(document) = json_object(input) else {
        return Ok(());
    };
    utf8_bound_violation(
        "focused_leaf_key",
        document.get("focused_leaf_key"),
        LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        LAYOUT_KEY_LABEL,
    )?;
    preflight_bindings(document)?;
    preflight_root(document)
}

fn preflight_bindings(document: &Map<String, Value>) -> ProtocolResult<()> {
    let Some(bindings) = array_value(document.get("bindings")) else {
        return Ok(());
    };
    if bindings.len() > LAYOUT_DOCUMENT_MAX_BINDINGS {
        return Err(ProtocolError::new(
            "bindings",
            format!("layout document exceeds {LAYOUT_DOCUMENT_MAX_BINDINGS} bindings"),
        ));
    }
    for (index, binding) in bindings.iter().enumerate() {
        let Some(candidate) = json_object(binding) else {
            continue;
        };
        utf8_bound_violation(
            &format!("bindings[{index}].slot_key"),
            candidate.get("slot_key"),
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
            LAYOUT_KEY_LABEL,
        )?;
        utf8_bound_violation(
            &format!("bindings[{index}].session_id"),
            candidate.get("session_id"),
            LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
            LAYOUT_SESSION_ID_LABEL,
        )?;
    }
    Ok(())
}

fn preflight_root(document: &Map<String, Value>) -> ProtocolResult<()> {
    let mut pending = vec![PendingLayoutNode {
        node: document.get("root").unwrap_or(&Value::Null),
        path: String::from("root"),
        depth: 1,
    }];
    let mut nodes = 0;
    let mut slots = 0;
    while let Some(current) = pending.pop() {
        if current.depth > LAYOUT_DOCUMENT_MAX_DEPTH {
            return Err(ProtocolError::new(
                &current.path,
                format!("layout document exceeds depth {LAYOUT_DOCUMENT_MAX_DEPTH}"),
            ));
        }
        let Some(node) = json_object(current.node) else {
            continue;
        };
        let kind = node.get("kind").and_then(Value::as_str);
        if kind != Some("leaf") && kind != Some("split") {
            continue;
        }
        nodes += 1;
        if nodes > LAYOUT_DOCUMENT_MAX_NODES {
            return Err(ProtocolError::new(
                &current.path,
                format!("layout document exceeds {LAYOUT_DOCUMENT_MAX_NODES} nodes"),
            ));
        }
        if kind == Some("split") {
            // Pushed second-first so the walk reports a violation in the same
            // preorder the parse would.
            pending.push(split_child(&current, node, "second"));
            pending.push(split_child(&current, node, "first"));
            continue;
        }
        utf8_bound_violation(
            &format!("{}.leaf_key", current.path),
            node.get("leaf_key"),
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
            LAYOUT_KEY_LABEL,
        )?;
        utf8_bound_violation(
            &format!("{}.selected_slot_key", current.path),
            node.get("selected_slot_key"),
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
            LAYOUT_KEY_LABEL,
        )?;
        // The check inside bounds the addition to the remaining budget, so the
        // running total cannot pass the document-wide slot cap.
        slots += slot_key_violations(&current, node, slots)?;
    }
    Ok(())
}

fn split_child<'a>(
    current: &PendingLayoutNode<'a>,
    node: &'a Map<String, Value>,
    side: &str,
) -> PendingLayoutNode<'a> {
    PendingLayoutNode {
        node: node.get(side).unwrap_or(&Value::Null),
        path: format!("{}.{side}", current.path),
        depth: current.depth + 1,
    }
}

/// Check one leaf's slots against the document-wide budget, returning its count.
fn slot_key_violations(
    current: &PendingLayoutNode<'_>,
    node: &Map<String, Value>,
    slots: usize,
) -> ProtocolResult<usize> {
    let Some(slot_keys) = array_value(node.get("slot_keys")) else {
        return Ok(0);
    };
    if slot_keys.len() > LAYOUT_DOCUMENT_MAX_SLOTS - slots {
        return Err(ProtocolError::new(
            format!("{}.slot_keys", current.path),
            format!("layout document exceeds {LAYOUT_DOCUMENT_MAX_SLOTS} slots"),
        ));
    }
    for (index, slot_key) in slot_keys.iter().enumerate() {
        utf8_bound_violation(
            &format!("{}.slot_keys[{index}]", current.path),
            Some(slot_key),
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
            LAYOUT_KEY_LABEL,
        )?;
    }
    Ok(slot_keys.len())
}

fn utf8_bound_violation(
    path: &str,
    value: Option<&Value>,
    max_bytes: usize,
    label: &str,
) -> ProtocolResult<()> {
    match value.and_then(Value::as_str) {
        Some(text) if text.len() > max_bytes => Err(ProtocolError::new(
            path,
            format!("{label} must not exceed {max_bytes} UTF-8 bytes"),
        )),
        _ => Ok(()),
    }
}

fn json_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn array_value(value: Option<&Value>) -> Option<&Vec<Value>> {
    value.and_then(Value::as_array)
}

// The strict readers below are `document`'s, not the preflight's: this is the
// first boundary untrusted layout JSON crosses, and `document` already depends
// on this module, so the definitions live here rather than in a second copy.

/// A JSON object, or the refusal a shape check reports.
pub(crate) fn strict_object<'a>(
    value: &'a Value,
    path: &str,
) -> ProtocolResult<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| ProtocolError::new(path, "must be a JSON object"))
}

/// A required string field, read in place.
pub(crate) fn read_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> ProtocolResult<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ProtocolError::new(join(path, key), "must be a string"))
}

/// A required array field, read in place.
pub(crate) fn read_array<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> ProtocolResult<&'a Vec<Value>> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::new(join(path, key), "must be a JSON array"))
}

/// A field that must carry exactly the number one, whatever JSON spelling
/// spells one: `1`, `1.0` and `1e0` are the same version.
pub(crate) fn read_literal_one(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> ProtocolResult<u8> {
    match object.get(key).and_then(Value::as_f64) {
        Some(1.0) => Ok(1),
        _ => Err(ProtocolError::new(join(path, key), "must be 1")),
    }
}

/// Refuse any key the object does not declare, so a document carrying an extra
/// field is not silently accepted by a schema that would have rejected it.
pub(crate) fn reject_unknown_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> ProtocolResult<()> {
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(unknown) => Err(ProtocolError::new(
            join(path, unknown),
            "is not a layout document field",
        )),
        None => Ok(()),
    }
}

/// Join a path and a key for an error that has to name the offending field.
pub(crate) fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        String::from(key)
    } else {
        format!("{path}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        preflight_layout_document_resources,
    };

    fn leaf(leaf_key: &str) -> serde_json::Value {
        json!({ "kind": "leaf", "leaf_key": leaf_key, "slot_keys": [], "selected_slot_key": null })
    }

    /// Assemble a node by inserting into a map directly. The `json!` macro
    /// would not do: an interpolated `Value` inside it goes through
    /// `to_value`, which re-serializes the whole subtree and recurses once per
    /// level. A depth test built that way measures the test's own constructor
    /// rather than the preflight, and overflows before the preflight is called.
    fn split_node(first: serde_json::Value, second: serde_json::Value) -> serde_json::Value {
        let mut node = serde_json::Map::new();
        node.insert("kind".to_owned(), serde_json::Value::from("split"));
        node.insert("direction".to_owned(), serde_json::Value::from("row"));
        node.insert("ratio".to_owned(), serde_json::Value::from(0.5));
        node.insert("first".to_owned(), first);
        node.insert("second".to_owned(), second);
        serde_json::Value::Object(node)
    }

    fn document(root: serde_json::Value) -> serde_json::Value {
        let mut envelope = serde_json::Map::new();
        envelope.insert("schema_version".to_owned(), serde_json::Value::from(1));
        envelope.insert("root".to_owned(), root);
        envelope.insert(
            "focused_leaf_key".to_owned(),
            serde_json::Value::from("deep-leaf"),
        );
        envelope.insert("bindings".to_owned(), serde_json::Value::Array(Vec::new()));
        serde_json::Value::Object(envelope)
    }

    fn document_of_depth(depth: usize) -> serde_json::Value {
        let mut root = leaf("deep-leaf");
        for level in 2..=depth {
            root = split_node(leaf(&format!("side-{level}")), root);
        }
        document(root)
    }

    #[test]
    fn a_value_that_is_not_a_document_is_left_to_the_parse() {
        assert!(preflight_layout_document_resources(&json!(null)).is_ok());
        assert!(preflight_layout_document_resources(&json!("root")).is_ok());
        assert!(preflight_layout_document_resources(&json!([1, 2, 3])).is_ok());
        assert!(preflight_layout_document_resources(&json!({})).is_ok());
    }

    #[test]
    fn a_hostile_depth_is_bounded_without_recursing() {
        // 10_000 levels would overflow a recursive walk, and the preflight is
        // the only thing standing between that document and the parse. The
        // value is leaked rather than dropped: `serde_json::Value`'s own `Drop`
        // recurses once per level, so dropping a tree this deep would abort the
        // test thread before the assertion could report anything. No parser can
        // hand the preflight a value this deep — serde_json's own nesting limit
        // is far lower — which is why the preflight, not the parser, is what
        // this test is about.
        let hostile = document_of_depth(10_000);
        let violation = preflight_layout_document_resources(&hostile)
            .expect_err("a document deeper than the cap is refused");
        std::mem::forget(hostile);
        assert!(
            violation.reason.contains("exceeds depth"),
            "unexpected violation: {violation}"
        );
    }

    #[test]
    fn a_key_one_byte_over_its_bound_is_refused() {
        let exact = "k".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES);
        let at_bound = document(leaf(&exact));
        assert!(preflight_layout_document_resources(&at_bound).is_ok());
        let over = document(leaf(&format!("{exact}k")));
        assert!(preflight_layout_document_resources(&over).is_err());
    }

    #[test]
    fn the_depth_bound_admits_a_document_exactly_at_it() {
        let deepest = document_of_depth(LAYOUT_DOCUMENT_MAX_DEPTH);
        let admitted = preflight_layout_document_resources(&deepest);
        assert!(admitted.is_ok(), "unexpected violation: {admitted:?}");
    }
}
