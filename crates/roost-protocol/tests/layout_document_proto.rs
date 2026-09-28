//! The layout document's protobuf adapter: the recursive mapping both ways,
//! the null-selection encoding, every malformed message it must refuse, and
//! the iterative resource preflight that runs before any recursion.
//!
//! Ports `packages/protocol/tests/layout-document-proto.test.ts` and the
//! protobuf half of `layout-document-bounds.test.ts`. The bounds test's cyclic
//! message has no Rust form: a protobuf tree here is owned, so a node cannot
//! be its own child.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod layout_proto_support;
mod support;

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

use support::{SESSION_A, SESSION_B, document, leaf, split};

use layout_proto_support::*;

#[test]
fn round_trips_nested_trees_empty_leaves_null_selection_and_ratio_endpoints() {
    let source = nested_document();
    let mut wire = layout_document_to_proto(&source).unwrap();
    assert_eq!(layout_document_from_proto(&wire).unwrap(), source);

    let split = root_split(&mut wire);
    assert_eq!(split.direction, proto::LayoutDirection::Row);
    assert_eq!(split.ratio, LAYOUT_RATIO_MIN);
    let Some(Node::Split(nested)) = split.second.as_option().and_then(|node| node.node.as_ref())
    else {
        panic!("expected a nested split");
    };
    assert_eq!(nested.direction, proto::LayoutDirection::Col);
    assert_eq!(nested.ratio, LAYOUT_RATIO_MAX);
    let Some(Node::Leaf(empty)) = nested.first.as_option().and_then(|node| node.node.as_ref())
    else {
        panic!("expected the empty leaf");
    };
    assert_eq!(empty.leaf_key, "leaf-empty");
    assert!(empty.slot_keys.is_empty());
    assert_eq!(empty.selected_slot_key, None);
}

#[test]
fn to_proto_revalidates_a_hand_built_document_before_mapping() {
    let mut invalid = nested_document();
    invalid.focused_leaf_key = "leaf-missing".to_owned();
    assert!(layout_document_to_proto(&invalid).is_err());
}

#[test]
fn refuses_a_missing_root_node_shape_or_split_child() {
    let mut missing_root = occupied_leaf_proto();
    missing_root.root = MessageField::none();
    assert_refused(&missing_root);

    let mut missing_node = occupied_leaf_proto();
    missing_node.root = MessageField::some(proto::LayoutDocumentNode::default());
    assert_refused(&missing_node);

    let mut missing_first = empty_split_proto(row(), 0.5);
    root_split(&mut missing_first).first = MessageField::none();
    assert_refused(&missing_first);

    let mut missing_second = empty_split_proto(row(), 0.5);
    root_split(&mut missing_second).second = MessageField::none();
    assert_refused(&missing_second);
}

#[test]
fn refuses_unspecified_and_unknown_split_directions() {
    assert_refused(&empty_split_proto(
        proto::LayoutDirection::Unspecified.into(),
        0.5,
    ));
    assert_refused(&empty_split_proto(EnumValue::from(99), 0.5));
}

#[test]
fn refuses_invalid_versions_and_split_ratios() {
    for schema_version in [0, 2] {
        let mut document = occupied_leaf_proto();
        document.schema_version = schema_version;
        assert_refused(&document);
    }
    for ratio in [
        LAYOUT_RATIO_MIN - f64::EPSILON,
        LAYOUT_RATIO_MAX + f64::EPSILON,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert_refused(&empty_split_proto(row(), ratio));
    }
}

#[test]
fn refuses_duplicate_and_missing_bindings() {
    let mut duplicate = occupied_leaf_proto();
    duplicate.bindings.push(binding("slot-a", SESSION_B));
    assert_refused(&duplicate);

    let mut missing = occupied_leaf_proto();
    missing.bindings.clear();
    assert_refused(&missing);
}

#[test]
fn refuses_empty_leaf_slot_focus_binding_and_session_ids() {
    let mut empty_leaf_key = occupied_leaf_proto();
    root_leaf(&mut empty_leaf_key).leaf_key = String::new();
    assert_refused(&empty_leaf_key);

    let mut empty_slot_key = occupied_leaf_proto();
    root_leaf(&mut empty_slot_key).slot_keys = vec![String::new()];
    assert_refused(&empty_slot_key);

    let mut empty_selection = occupied_leaf_proto();
    root_leaf(&mut empty_selection).selected_slot_key = Some(String::new());
    assert_refused(&empty_selection);

    let mut empty_focus = occupied_leaf_proto();
    empty_focus.focused_leaf_key = String::new();
    assert_refused(&empty_focus);

    let mut empty_binding_slot = occupied_leaf_proto();
    empty_binding_slot.bindings[0].slot_key = String::new();
    assert_refused(&empty_binding_slot);

    let mut empty_session = occupied_leaf_proto();
    empty_session.bindings[0].session_id = String::new();
    assert_refused(&empty_session);
}
