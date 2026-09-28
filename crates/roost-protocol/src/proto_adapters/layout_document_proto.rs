//! The portable layout document between its validated shape and the protobuf
//! `roost.v1.LayoutDocumentV1` the Sync and coordinator wires carry.
//!
//! Ports `packages/protocol/src/layout-document-proto.ts`: the browser, the
//! CLI and the coordinator share this one adapter. Both directions go through
//! `parse_layout_document_v1`, so the wire shape is validated by one parser,
//! and the protobuf side runs an ITERATIVE bounds walk before anything recurses
//! into a tree an untrusted peer built.

use roost_proto as proto;
use roost_proto::__buffa::oneof::layout_document_node::Node;
use serde_json::{Value, json};

use crate::error::{ProtocolError, ProtocolResult};
use crate::layout::document::{
    LayoutDirection, LayoutDocumentNode, LayoutDocumentV1, parse_layout_document_v1,
};
use crate::layout::preflight::{
    LAYOUT_DOCUMENT_MAX_BINDINGS, LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_NODES, LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_SLOTS,
};

const ERROR_FIELD: &str = "layout_document";

/// A validated document as the wire carries it.
///
/// Revalidates first: a hand-built value that never went through the parser
/// (a focus naming a missing leaf, a ratio out of range) must not reach a peer
/// as if it were an arrangement.
pub fn layout_document_to_proto(
    document: &LayoutDocumentV1,
) -> ProtocolResult<proto::LayoutDocumentV1> {
    let value = serde_json::to_value(document)
        .map_err(|error| ProtocolError::new(ERROR_FIELD, error.to_string()))?;
    let checked = parse_layout_document_v1(&value)?;
    Ok(proto::LayoutDocumentV1 {
        schema_version: u32::from(checked.schema_version),
        root: roost_proto::buffa::MessageField::some(node_to_proto(&checked.root)),
        focused_leaf_key: checked.focused_leaf_key,
        bindings: checked
            .bindings
            .into_iter()
            .map(|binding| proto::LayoutDocumentBinding {
                slot_key: binding.slot_key,
                session_id: binding.session_id,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

/// The wire document as the shared parser accepts it.
///
/// An absent root, a node naming no shape, an absent split child, and an
/// unspecified or unknown direction all map to `null`, which the parser refuses
/// with the field path -- the adapter never decides validity itself.
pub fn layout_document_from_proto(
    document: &proto::LayoutDocumentV1,
) -> ProtocolResult<LayoutDocumentV1> {
    preflight_proto_document(document)?;
    let bindings: Vec<Value> = document
        .bindings
        .iter()
        .map(|binding| json!({ "slot_key": binding.slot_key, "session_id": binding.session_id }))
        .collect();
    parse_layout_document_v1(&json!({
        "schema_version": document.schema_version,
        "root": node_from_proto(document.root.as_option()),
        "focused_leaf_key": document.focused_leaf_key,
        "bindings": bindings,
    }))
}

/// Validate a document off the wire and return the canonical protobuf copy.
///
/// The copy is rebuilt field by field, so unknown fields a caller appended are
/// dropped rather than relayed; the coordinator retains, publishes and forwards
/// this copy (v2 `apps/coord/src/ui-state/handlers-ui.ts:66,120`).
pub fn canonical_layout_document(
    document: &proto::LayoutDocumentV1,
) -> ProtocolResult<proto::LayoutDocumentV1> {
    layout_document_to_proto(&layout_document_from_proto(document)?)
}

fn node_to_proto(node: &LayoutDocumentNode) -> proto::LayoutDocumentNode {
    let node = match node {
        LayoutDocumentNode::Leaf(leaf) => Node::Leaf(Box::new(proto::LayoutDocumentLeaf {
            leaf_key: leaf.leaf_key.clone(),
            slot_keys: leaf.slot_keys.clone(),
            selected_slot_key: leaf.selected_slot_key.clone(),
            ..Default::default()
        })),
        LayoutDocumentNode::Split(split) => Node::Split(Box::new(proto::LayoutDocumentSplit {
            direction: match split.direction {
                LayoutDirection::Row => proto::LayoutDirection::Row,
                LayoutDirection::Col => proto::LayoutDirection::Col,
                // Unreachable after the parse, which refuses any other direction.
                LayoutDirection::Other(_) => proto::LayoutDirection::Unspecified,
            }
            .into(),
            ratio: split.ratio,
            first: roost_proto::buffa::MessageField::some(node_to_proto(&split.first)),
            second: roost_proto::buffa::MessageField::some(node_to_proto(&split.second)),
            ..Default::default()
        })),
    };
    proto::LayoutDocumentNode {
        node: Some(node),
        ..Default::default()
    }
}

/// Recursive, and safe to be: the preflight has already bounded the depth.
fn node_from_proto(node: Option<&proto::LayoutDocumentNode>) -> Value {
    match node.and_then(|node| node.node.as_ref()) {
        Some(Node::Leaf(leaf)) => json!({
            "kind": "leaf",
            "leaf_key": leaf.leaf_key,
            "slot_keys": leaf.slot_keys,
            "selected_slot_key": leaf.selected_slot_key,
        }),
        Some(Node::Split(split)) => {
            let direction = match split.direction.as_known() {
                Some(proto::LayoutDirection::Row) => Value::from("row"),
                Some(proto::LayoutDirection::Col) => Value::from("col"),
                _ => Value::Null,
            };
            json!({
                "kind": "split",
                "direction": direction,
                "ratio": split.ratio,
                "first": node_from_proto(split.first.as_option()),
                "second": node_from_proto(split.second.as_option()),
            })
        }
        None => Value::Null,
    }
}

/// The bounds walk over the protobuf tree, before anything recurses into it.
fn preflight_proto_document(document: &proto::LayoutDocumentV1) -> ProtocolResult<()> {
    check_utf8_bound(
        &document.focused_leaf_key,
        LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        "layout key",
    )?;
    if document.bindings.len() > LAYOUT_DOCUMENT_MAX_BINDINGS {
        return Err(exceeds(format!(
            "layout document exceeds {LAYOUT_DOCUMENT_MAX_BINDINGS} bindings"
        )));
    }
    for binding in &document.bindings {
        check_utf8_bound(
            &binding.slot_key,
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
            "layout key",
        )?;
        check_utf8_bound(
            &binding.session_id,
            LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
            "layout session_id",
        )?;
    }
    let mut pending = vec![(document.root.as_option(), 1usize)];
    let mut nodes = 0usize;
    let mut slots = 0usize;
    while let Some((node, depth)) = pending.pop() {
        if depth > LAYOUT_DOCUMENT_MAX_DEPTH {
            return Err(exceeds(format!(
                "layout document exceeds depth {LAYOUT_DOCUMENT_MAX_DEPTH}"
            )));
        }
        let Some(shape) = node.and_then(|node| node.node.as_ref()) else {
            continue;
        };
        nodes += 1;
        if nodes > LAYOUT_DOCUMENT_MAX_NODES {
            return Err(exceeds(format!(
                "layout document exceeds {LAYOUT_DOCUMENT_MAX_NODES} nodes"
            )));
        }
        match shape {
            Node::Split(split) => {
                pending.push((split.second.as_option(), depth + 1));
                pending.push((split.first.as_option(), depth + 1));
            }
            Node::Leaf(leaf) => {
                check_utf8_bound(
                    &leaf.leaf_key,
                    LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
                    "layout key",
                )?;
                if let Some(selected) = &leaf.selected_slot_key {
                    check_utf8_bound(selected, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES, "layout key")?;
                }
                if leaf.slot_keys.len() > LAYOUT_DOCUMENT_MAX_SLOTS - slots {
                    return Err(exceeds(format!(
                        "layout document exceeds {LAYOUT_DOCUMENT_MAX_SLOTS} slots"
                    )));
                }
                slots += leaf.slot_keys.len();
                for slot_key in &leaf.slot_keys {
                    check_utf8_bound(slot_key, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES, "layout key")?;
                }
            }
        }
    }
    Ok(())
}

fn check_utf8_bound(value: &str, max_bytes: usize, label: &str) -> ProtocolResult<()> {
    if value.len() <= max_bytes {
        Ok(())
    } else {
        Err(exceeds(format!(
            "{label} must not exceed {max_bytes} UTF-8 bytes"
        )))
    }
}

fn exceeds(reason: String) -> ProtocolError {
    ProtocolError::new(ERROR_FIELD, reason)
}
