//! Protobuf-side fixtures shared by the layout adapter's two test binaries:
//! hand-built wire trees, depth/slot/node boundary documents and the refusal
//! reader. Each binary uses a subset, so the rest is dead code there.
#![allow(dead_code, unused_imports)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_proto as proto;
use roost_proto::__buffa::oneof::layout_document_node::Node;
use roost_proto::buffa::{EnumValue, MessageField};
use roost_protocol::layout::document::{
    LAYOUT_RATIO_MAX, LAYOUT_RATIO_MIN, LayoutDocumentV1, parse_layout_document_v1,
};
use roost_protocol::layout::preflight::{
    LAYOUT_DOCUMENT_MAX_BINDINGS, LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_NODES, LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_SLOTS,
};
use roost_protocol::proto_adapters::layout_document_proto::{
    layout_document_from_proto, layout_document_to_proto,
};
use serde_json::{Value, json};

use super::support::{SESSION_A, SESSION_B, document, leaf, split};

pub(crate) fn typed(value: Value) -> LayoutDocumentV1 {
    parse_layout_document_v1(&value).unwrap()
}

pub(crate) fn nested_document() -> LayoutDocumentV1 {
    typed(json!({
        "schema_version": 1,
        "root": {
            "kind": "split", "direction": "row", "ratio": LAYOUT_RATIO_MIN,
            "first": leaf("leaf-a", &["slot-a"], Some("slot-a")),
            "second": {
                "kind": "split", "direction": "col", "ratio": LAYOUT_RATIO_MAX,
                "first": leaf("leaf-empty", &[], None),
                "second": leaf("leaf-b", &["slot-b"], Some("slot-b")),
            },
        },
        "focused_leaf_key": "leaf-empty",
        "bindings": [
            { "slot_key": "slot-a", "session_id": SESSION_A },
            { "slot_key": "slot-b", "session_id": SESSION_B },
        ],
    }))
}

pub(crate) fn leaf_node(
    leaf_key: &str,
    slot_keys: &[&str],
    selected: Option<&str>,
) -> proto::LayoutDocumentNode {
    proto::LayoutDocumentNode {
        node: Some(Node::Leaf(Box::new(proto::LayoutDocumentLeaf {
            leaf_key: leaf_key.to_owned(),
            slot_keys: slot_keys.iter().map(|key| (*key).to_owned()).collect(),
            selected_slot_key: selected.map(str::to_owned),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

pub(crate) fn split_node(
    direction: EnumValue<proto::LayoutDirection>,
    ratio: f64,
    first: Option<proto::LayoutDocumentNode>,
    second: Option<proto::LayoutDocumentNode>,
) -> proto::LayoutDocumentNode {
    let field = |node: Option<proto::LayoutDocumentNode>| {
        node.map_or(MessageField::none(), MessageField::some)
    };
    proto::LayoutDocumentNode {
        node: Some(Node::Split(Box::new(proto::LayoutDocumentSplit {
            direction,
            ratio,
            first: field(first),
            second: field(second),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

pub(crate) fn binding(slot_key: &str, session_id: &str) -> proto::LayoutDocumentBinding {
    proto::LayoutDocumentBinding {
        slot_key: slot_key.to_owned(),
        session_id: session_id.to_owned(),
        ..Default::default()
    }
}

pub(crate) fn occupied_leaf_proto() -> proto::LayoutDocumentV1 {
    proto::LayoutDocumentV1 {
        schema_version: 1,
        root: MessageField::some(leaf_node("leaf-a", &["slot-a"], Some("slot-a"))),
        focused_leaf_key: "leaf-a".to_owned(),
        bindings: vec![binding("slot-a", SESSION_A)],
        ..Default::default()
    }
}

pub(crate) fn empty_split_proto(
    direction: EnumValue<proto::LayoutDirection>,
    ratio: f64,
) -> proto::LayoutDocumentV1 {
    proto::LayoutDocumentV1 {
        schema_version: 1,
        root: MessageField::some(split_node(
            direction,
            ratio,
            Some(leaf_node("leaf-a", &[], None)),
            Some(leaf_node("leaf-b", &[], None)),
        )),
        focused_leaf_key: "leaf-a".to_owned(),
        ..Default::default()
    }
}

pub(crate) fn root_leaf(document: &mut proto::LayoutDocumentV1) -> &mut proto::LayoutDocumentLeaf {
    match document
        .root
        .as_option_mut()
        .and_then(|root| root.node.as_mut())
    {
        Some(Node::Leaf(leaf)) => leaf,
        _ => panic!("expected a root leaf"),
    }
}

pub(crate) fn root_split(
    document: &mut proto::LayoutDocumentV1,
) -> &mut proto::LayoutDocumentSplit {
    match document
        .root
        .as_option_mut()
        .and_then(|root| root.node.as_mut())
    {
        Some(Node::Split(split)) => split,
        _ => panic!("expected a root split"),
    }
}

pub(crate) fn assert_refused(document: &proto::LayoutDocumentV1) {
    assert!(
        layout_document_from_proto(document).is_err(),
        "{document:?} was accepted"
    );
}

pub(crate) fn refusal(document: &proto::LayoutDocumentV1) -> String {
    layout_document_from_proto(document)
        .unwrap_err()
        .to_string()
}

pub(crate) fn row() -> EnumValue<proto::LayoutDirection> {
    proto::LayoutDirection::Row.into()
}

pub(crate) fn depth_document(depth: usize) -> LayoutDocumentV1 {
    let mut root = leaf("deep-leaf", &[], None);
    for level in 2..=depth {
        root = split(leaf(&format!("side-{level}"), &[], None), root);
    }
    parse_layout_document_v1(&document(root, "deep-leaf", json!([]))).unwrap()
}

pub(crate) fn proto_depth_document(depth: usize) -> proto::LayoutDocumentV1 {
    let mut root = leaf_node("deep-leaf", &[], None);
    for level in 2..=depth {
        root = split_node(
            row(),
            0.5,
            Some(leaf_node(&format!("side-{level}"), &[], None)),
            Some(root),
        );
    }
    proto::LayoutDocumentV1 {
        schema_version: 1,
        root: MessageField::some(root),
        focused_leaf_key: "deep-leaf".to_owned(),
        ..Default::default()
    }
}

pub(crate) fn complete_tree(depth: u32, next_key: &mut usize) -> Value {
    if depth == 1 {
        let key = format!("leaf-{next_key}");
        *next_key += 1;
        return leaf(&key, &[], None);
    }
    let first = complete_tree(depth - 1, next_key);
    split(first, complete_tree(depth - 1, next_key))
}

pub(crate) fn exact_node_document() -> LayoutDocumentV1 {
    let complete_depth = (LAYOUT_DOCUMENT_MAX_NODES + 1).ilog2();
    typed(document(
        complete_tree(complete_depth, &mut 0),
        "leaf-0",
        json!([]),
    ))
}

pub(crate) fn document_with_slots(slot_count: usize) -> Value {
    let slot_keys: Vec<String> = (0..slot_count)
        .map(|index| format!("slot-{index}"))
        .collect();
    let bindings: Vec<Value> = (0..slot_count)
        .map(|index| json!({ "slot_key": format!("slot-{index}"), "session_id": format!("session-{index}") }))
        .collect();
    let slot_refs: Vec<&str> = slot_keys.iter().map(String::as_str).collect();
    document(
        leaf("leaf-slots", &slot_refs, slot_refs.first().copied()),
        "leaf-slots",
        Value::Array(bindings),
    )
}
