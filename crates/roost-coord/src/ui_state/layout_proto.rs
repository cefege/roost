//! Converts the portable layout document between its protobuf and validated
//! shapes, and is the coordinator's copy of v2's `layout-document-proto.ts`.
//! The coordinator, the browser and the CLI share that adapter in v2; here it
//! is the UI ingress path's, and it is a mechanical field mapping only -- every
//! bound, the preflight and the graph validation stay in
//! `roost_protocol::layout`, so the wire shape is validated by one parser.
//!
//! THE PROTO-SIDE PREFLIGHT IS NOT OPTIONAL. The conversion below recurses, and
//! the document arrives from an untrusted caller, so the iterative bounds walk
//! runs FIRST and is the only thing standing between a deep or wide document and
//! the stack. v2 asserts the same bounds before it converts
//! (`layout-document-proto.ts:110-163`).

use roost_proto as proto;
use roost_proto::__buffa::oneof::layout_document_node::Node;
use roost_protocol::ProtocolResult;
use roost_protocol::error::ProtocolError;
use roost_protocol::layout::document::{LayoutDirection, LayoutNodeKind};
use roost_protocol::layout::{
    LAYOUT_DOCUMENT_MAX_BINDINGS, LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_NODES, LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_SLOTS, LayoutDocumentBinding, LayoutDocumentLeaf, LayoutDocumentNode,
    LayoutDocumentSplit, LayoutDocumentV1, parse_layout_document_v1,
};
use roost_protocol::validate::max_utf8_bytes;

/// Validate a document off the wire and return the canonical protobuf copy.
///
/// The canonical copy is what is retained, published and sent to a target tab:
/// it is rebuilt field by field, so unknown fields a caller appended to the wire
/// are dropped rather than relayed (`ui-handler-hardening.test.ts:107-166`).
pub fn canonical_layout_document(
    document: &proto::LayoutDocumentV1,
) -> ProtocolResult<proto::LayoutDocumentV1> {
    preflight_proto_document(document)?;
    let mapped = layout_document_from_proto(document)?;
    let checked =
        parse_layout_document_v1(&to_value(mapped)?).map_err(|error| invalid(error.to_string()))?;
    layout_document_to_proto(&checked)
}

/// The wire document as the shared parser sees it.
pub fn layout_document_from_proto(
    document: &proto::LayoutDocumentV1,
) -> ProtocolResult<LayoutDocumentV1> {
    let schema_version = u8::try_from(document.schema_version).map_err(|_| {
        invalid(format!(
            "layout schema_version {} is not a portable version",
            document.schema_version
        ))
    })?;
    Ok(LayoutDocumentV1 {
        schema_version,
        root: node_from_proto(document.root.as_option())?,
        focused_leaf_key: document.focused_leaf_key.clone(),
        bindings: document
            .bindings
            .iter()
            .map(|binding| LayoutDocumentBinding {
                slot_key: binding.slot_key.clone(),
                session_id: binding.session_id.clone(),
            })
            .collect(),
    })
}

/// A validated document as the wire carries it.
pub fn layout_document_to_proto(
    document: &LayoutDocumentV1,
) -> ProtocolResult<proto::LayoutDocumentV1> {
    let rechecked = parse_layout_document_v1(&to_value(document)?)
        .map_err(|error| invalid(error.to_string()))?;
    Ok(proto::LayoutDocumentV1 {
        schema_version: u32::from(rechecked.schema_version),
        root: roost_proto::buffa::MessageField::some(node_to_proto(&rechecked.root)),
        focused_leaf_key: rechecked.focused_leaf_key,
        bindings: rechecked
            .bindings
            .iter()
            .map(|binding| proto::LayoutDocumentBinding {
                slot_key: binding.slot_key.clone(),
                session_id: binding.session_id.clone(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

fn to_value<T: serde::Serialize>(value: T) -> ProtocolResult<serde_json::Value> {
    serde_json::to_value(value).map_err(|error| invalid(error.to_string()))
}

fn invalid(reason: impl Into<String>) -> ProtocolError {
    ProtocolError::new("layout_document", reason.into())
}

fn node_from_proto(node: Option<&proto::LayoutDocumentNode>) -> ProtocolResult<LayoutDocumentNode> {
    let node = node.ok_or_else(|| invalid("layout document has no root node"))?;
    match node.node.as_ref() {
        Some(Node::Leaf(leaf)) => Ok(LayoutDocumentNode::Leaf(LayoutDocumentLeaf {
            kind: LayoutNodeKind::Leaf,
            leaf_key: leaf.leaf_key.clone(),
            slot_keys: leaf.slot_keys.clone(),
            selected_slot_key: leaf.selected_slot_key.clone(),
        })),
        Some(Node::Split(split)) => {
            let direction = match split.direction.as_known() {
                Some(proto::LayoutDirection::Row) => LayoutDirection::Row,
                Some(proto::LayoutDirection::Col) => LayoutDirection::Col,
                _ => {
                    return Err(invalid(
                        "layout split direction is unset or not a portable direction",
                    ));
                }
            };
            Ok(LayoutDocumentNode::Split(LayoutDocumentSplit {
                kind: LayoutNodeKind::Split,
                direction,
                ratio: split.ratio,
                first: Box::new(node_from_proto(split.first.as_option())?),
                second: Box::new(node_from_proto(split.second.as_option())?),
            }))
        }
        None => Err(invalid("layout document node names no shape")),
    }
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

/// The bounds walk, over the protobuf tree, before anything recurses into it.
fn preflight_proto_document(document: &proto::LayoutDocumentV1) -> ProtocolResult<()> {
    max_utf8_bytes(
        "focused_leaf_key",
        &document.focused_leaf_key,
        LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    )
    .map_err(|error| invalid(error.to_string()))?;
    if document.bindings.len() > LAYOUT_DOCUMENT_MAX_BINDINGS {
        return Err(invalid(format!(
            "layout document exceeds {LAYOUT_DOCUMENT_MAX_BINDINGS} bindings"
        )));
    }
    for binding in &document.bindings {
        max_utf8_bytes(
            "binding.slot_key",
            &binding.slot_key,
            LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        )
        .map_err(|error| invalid(error.to_string()))?;
        max_utf8_bytes(
            "binding.session_id",
            &binding.session_id,
            LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
        )
        .map_err(|error| invalid(error.to_string()))?;
    }
    let mut pending = vec![(document.root.as_option(), 1usize)];
    let mut nodes = 0usize;
    let mut slots = 0usize;
    while let Some((node, depth)) = pending.pop() {
        if depth > LAYOUT_DOCUMENT_MAX_DEPTH {
            return Err(invalid(format!(
                "layout document exceeds depth {LAYOUT_DOCUMENT_MAX_DEPTH}"
            )));
        }
        let Some(node) = node else {
            continue;
        };
        nodes += 1;
        if nodes > LAYOUT_DOCUMENT_MAX_NODES {
            return Err(invalid(format!(
                "layout document exceeds {LAYOUT_DOCUMENT_MAX_NODES} nodes"
            )));
        }
        match node.node.as_ref() {
            Some(Node::Split(split)) => {
                pending.push((split.second.as_option(), depth + 1));
                pending.push((split.first.as_option(), depth + 1));
            }
            Some(Node::Leaf(leaf)) => {
                check_key("leaf.leaf_key", &leaf.leaf_key)?;
                if let Some(selected) = &leaf.selected_slot_key {
                    check_key("leaf.selected_slot_key", selected)?;
                }
                if leaf.slot_keys.len() > LAYOUT_DOCUMENT_MAX_SLOTS - slots {
                    return Err(invalid(format!(
                        "layout document exceeds {LAYOUT_DOCUMENT_MAX_SLOTS} slots"
                    )));
                }
                slots += leaf.slot_keys.len();
                for slot_key in &leaf.slot_keys {
                    check_key("leaf.slot_key", slot_key)?;
                }
            }
            None => {}
        }
    }
    Ok(())
}

fn check_key(field: &str, value: &str) -> ProtocolResult<()> {
    max_utf8_bytes(field, value, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES)
        .map_err(|error| invalid(error.to_string()))
}
