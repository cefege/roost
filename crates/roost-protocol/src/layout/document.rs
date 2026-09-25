//! The layout document types and the strict parse that admits them.
//!
//! A layout document is untrusted input: it arrives from a browser, a CLI flag
//! or a file on disk. `preflight` bounds its work, this file parses it, and
//! `graph` then proves the tree is internally consistent. Both walks are
//! worklists over the same depth cap, so a hostile document is refused by the
//! cheap pass before a tree is built for it at all.

mod graph;

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ProtocolResult;
use crate::error::ProtocolError;
use crate::layout::preflight::{
    LAYOUT_DOCUMENT_MAX_BINDINGS, LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES, join, preflight_layout_document_resources,
    read_array, read_literal_one, read_string, reject_unknown_keys, strict_object,
};
use crate::validate::{max_utf8_bytes, non_empty, one_of};

/// A split's smaller side, as a fraction of the parent.
pub const LAYOUT_RATIO_MIN: f64 = 0.1;
/// A split's larger side, as a fraction of the parent.
pub const LAYOUT_RATIO_MAX: f64 = 0.9;

/// The tag that says which node shape a value is. The union is closed on
/// purpose: a kind this build cannot render is a document it must refuse, not
/// one it must decode and then guess at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutNodeKind {
    Leaf,
    Split,
}

/// Which way a split divides its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutDirection {
    Row,
    Col,
    /// A direction a newer peer wrote, carried so its document still decodes.
    /// `parse_layout_document_v1` refuses it rather than rendering it wrongly.
    Other(String),
}

impl LayoutDirection {
    const WIRE_VALUES: [&'static str; 2] = ["row", "col"];

    pub fn as_wire(&self) -> &str {
        match self {
            Self::Row => "row",
            Self::Col => "col",
            Self::Other(value) => value,
        }
    }

    pub fn parse(field: &str, value: &str) -> ProtocolResult<Self> {
        one_of(field, value, &Self::WIRE_VALUES)?;
        Ok(if value == "row" { Self::Row } else { Self::Col })
    }
}

impl Serialize for LayoutDirection {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for LayoutDirection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(deserializer)?;
        Ok(match wire.as_str() {
            "row" => Self::Row,
            "col" => Self::Col,
            _ => Self::Other(wire),
        })
    }
}

/// One pane's attachment to one live session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct LayoutDocumentBinding {
    pub slot_key: String,
    /// A session id this document names, not one it proves is live: the
    /// importing folder checks it against its own canonical live set, because
    /// a document outlives the sessions it was exported against.
    pub session_id: String,
}

/// A pane, with the slots its tab strip can show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct LayoutDocumentLeaf {
    pub kind: LayoutNodeKind,
    pub leaf_key: String,
    pub slot_keys: Vec<String>,
    /// Null exactly when `slot_keys` is empty.
    pub selected_slot_key: Option<String>,
}

/// A divider between two subtrees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct LayoutDocumentSplit {
    pub kind: LayoutNodeKind,
    pub direction: LayoutDirection,
    pub ratio: f64,
    pub first: Box<LayoutDocumentNode>,
    pub second: Box<LayoutDocumentNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LayoutDocumentNode {
    Leaf(LayoutDocumentLeaf),
    Split(LayoutDocumentSplit),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct LayoutDocumentV1 {
    pub schema_version: u8,
    pub root: LayoutDocumentNode,
    pub focused_leaf_key: String,
    pub bindings: Vec<LayoutDocumentBinding>,
}

/// Parse an untrusted value into a layout document.
///
/// The preflight runs first and is why this cannot exhaust the stack: by the
/// time the worklist below walks the tree, the depth, the node count, the slot
/// count, the binding count and every key's byte length are already in bounds.
pub fn parse_layout_document_v1(input: &Value) -> ProtocolResult<LayoutDocumentV1> {
    preflight_layout_document_resources(input)?;
    let document = strict_object(input, "")?;
    reject_unknown_keys(
        document,
        &["schema_version", "root", "focused_leaf_key", "bindings"],
        "",
    )?;
    let schema_version = read_literal_one(document, "schema_version", "")?;
    let root = parse_node_tree(document.get("root").unwrap_or(&Value::Null))?;
    let focused_leaf_key = bounded_string(
        document.get("focused_leaf_key"),
        "focused_leaf_key",
        LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    )?;
    let parsed = LayoutDocumentV1 {
        schema_version,
        root,
        focused_leaf_key,
        bindings: parse_bindings(document.get("bindings").unwrap_or(&Value::Null))?,
    };
    graph::validate(&parsed)?;
    Ok(parsed)
}

/// Whether a value is a layout document this build accepts.
///
/// The TypeScript original is a plain success flag that never told a malformed
/// document from an oversized one, and so is this; a caller that needs that
/// distinction reads `ProtocolError::reason` from the parse.
pub fn is_layout_document_v1(input: &Value) -> bool {
    parse_layout_document_v1(input).is_ok()
}

/// A pending step in the tree walk: descend into a value, or join the two
/// children a split is waiting on.
enum ParseStep<'a> {
    Descend(&'a Value, String, usize),
    Join(LayoutDirection, f64),
}

fn parse_node_tree(root: &Value) -> ProtocolResult<LayoutDocumentNode> {
    let mut steps = vec![ParseStep::Descend(root, String::from("root"), 1)];
    let mut built: Vec<LayoutDocumentNode> = Vec::new();
    while let Some(step) = steps.pop() {
        match step {
            ParseStep::Descend(value, path, depth) => {
                if depth > LAYOUT_DOCUMENT_MAX_DEPTH {
                    return Err(ProtocolError::new(
                        &path,
                        format!("layout document exceeds depth {LAYOUT_DOCUMENT_MAX_DEPTH}"),
                    ));
                }
                let node = strict_object(value, &path)?;
                if read_node_kind(node, &path)? == LayoutNodeKind::Leaf {
                    built.push(LayoutDocumentNode::Leaf(parse_leaf(node, &path)?));
                    continue;
                }
                reject_unknown_keys(
                    node,
                    &["kind", "direction", "ratio", "first", "second"],
                    &path,
                )?;
                let direction = LayoutDirection::parse(
                    &join(&path, "direction"),
                    read_string(node, "direction", &path)?,
                )?;
                let ratio = read_ratio(node, &path)?;
                // Descend first-then-second and join on the way back up, so the
                // children are validated in the order a reader walks them.
                steps.push(ParseStep::Join(direction, ratio));
                steps.push(ParseStep::Descend(
                    node.get("second").unwrap_or(&Value::Null),
                    format!("{path}.second"),
                    depth + 1,
                ));
                steps.push(ParseStep::Descend(
                    node.get("first").unwrap_or(&Value::Null),
                    format!("{path}.first"),
                    depth + 1,
                ));
            }
            ParseStep::Join(direction, ratio) => {
                let second = take_node(&mut built)?;
                let first = take_node(&mut built)?;
                built.push(LayoutDocumentNode::Split(LayoutDocumentSplit {
                    kind: LayoutNodeKind::Split,
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }));
            }
        }
    }
    take_node(&mut built)
}

fn take_node(built: &mut Vec<LayoutDocumentNode>) -> ProtocolResult<LayoutDocumentNode> {
    built
        .pop()
        .ok_or_else(|| ProtocolError::new("root", "layout tree did not reduce to one node"))
}

fn parse_leaf(node: &Map<String, Value>, path: &str) -> ProtocolResult<LayoutDocumentLeaf> {
    reject_unknown_keys(
        node,
        &["kind", "leaf_key", "slot_keys", "selected_slot_key"],
        path,
    )?;
    let selected_path = join(path, "selected_slot_key");
    let selected_slot_key = match node.get("selected_slot_key") {
        // Null is the "this leaf has no slots" case, not a missing field.
        Some(Value::Null) => None,
        Some(selected) => Some(bounded_string(
            Some(selected),
            &selected_path,
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        )?),
        None => {
            return Err(ProtocolError::new(
                &selected_path,
                "must be present, as a layout key or null",
            ));
        }
    };
    Ok(LayoutDocumentLeaf {
        kind: LayoutNodeKind::Leaf,
        leaf_key: bounded_string(
            node.get("leaf_key"),
            &join(path, "leaf_key"),
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        )?,
        slot_keys: read_array(node, "slot_keys", path)?
            .iter()
            .enumerate()
            .map(|(index, slot_key)| {
                bounded_string(
                    Some(slot_key),
                    &format!("{path}.slot_keys[{index}]"),
                    LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
                )
            })
            .collect::<ProtocolResult<Vec<String>>>()?,
        selected_slot_key,
    })
}

fn parse_bindings(bindings: &Value) -> ProtocolResult<Vec<LayoutDocumentBinding>> {
    let entries = bindings
        .as_array()
        .ok_or_else(|| ProtocolError::new("bindings", "must be a JSON array"))?;
    if entries.len() > LAYOUT_DOCUMENT_MAX_BINDINGS {
        return Err(ProtocolError::new(
            "bindings",
            format!("layout document exceeds {LAYOUT_DOCUMENT_MAX_BINDINGS} bindings"),
        ));
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let path = format!("bindings[{index}]");
            let binding = strict_object(entry, &path)?;
            reject_unknown_keys(binding, &["slot_key", "session_id"], &path)?;
            Ok(LayoutDocumentBinding {
                slot_key: bounded_string(
                    binding.get("slot_key"),
                    &join(&path, "slot_key"),
                    LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
                )?,
                session_id: bounded_string(
                    binding.get("session_id"),
                    &join(&path, "session_id"),
                    LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
                )?,
            })
        })
        .collect()
}

fn bounded_string(value: Option<&Value>, path: &str, max_bytes: usize) -> ProtocolResult<String> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| ProtocolError::new(path, "must be a string"))?;
    non_empty(path, text)?;
    max_utf8_bytes(path, text, max_bytes)?;
    Ok(text.to_owned())
}

fn read_node_kind(node: &Map<String, Value>, path: &str) -> ProtocolResult<LayoutNodeKind> {
    match read_string(node, "kind", path)? {
        "leaf" => Ok(LayoutNodeKind::Leaf),
        "split" => Ok(LayoutNodeKind::Split),
        other => Err(ProtocolError::new(
            join(path, "kind"),
            format!("must be \"leaf\" or \"split\", got {other:?}"),
        )),
    }
}

fn read_ratio(node: &Map<String, Value>, path: &str) -> ProtocolResult<f64> {
    let field = join(path, "ratio");
    let ratio = node
        .get("ratio")
        .and_then(Value::as_f64)
        .ok_or_else(|| ProtocolError::new(&field, "must be a number"))?;
    // A NaN compares false against both bounds, so finiteness is checked first:
    // without it a non-finite ratio would pass the range check below.
    if !ratio.is_finite() || !(LAYOUT_RATIO_MIN..=LAYOUT_RATIO_MAX).contains(&ratio) {
        return Err(ProtocolError::new(
            &field,
            format!(
                "must be finite and within {LAYOUT_RATIO_MIN}..={LAYOUT_RATIO_MAX}, got {ratio}"
            ),
        ));
    }
    Ok(ratio)
}
