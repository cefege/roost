//! The layout adapter's resource bounds on the protobuf side: exact depth,
//! string, slot, binding and node-count boundaries round-trip, and one past
//! each is refused by the iterative preflight before anything recurses.
//!
//! Ports the protobuf half of `packages/protocol/tests/layout-document-bounds.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod layout_proto_support;
mod support;

use roost_proto::buffa::MessageField;
use roost_protocol::layout::preflight::{
    LAYOUT_DOCUMENT_MAX_BINDINGS, LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES, LAYOUT_DOCUMENT_MAX_SLOTS,
};
use roost_protocol::proto_adapters::layout_document_proto::{
    layout_document_from_proto, layout_document_to_proto,
};
use serde_json::Value;

use support::{leaf, split};

use layout_proto_support::*;

#[test]
fn round_trips_the_exact_depth_boundary() {
    let exact = depth_document(LAYOUT_DOCUMENT_MAX_DEPTH);
    let wire = layout_document_to_proto(&exact).unwrap();
    assert_eq!(layout_document_from_proto(&wire).unwrap(), exact);
    let built = proto_depth_document(LAYOUT_DOCUMENT_MAX_DEPTH);
    assert_eq!(layout_document_from_proto(&built).unwrap(), exact);
}

#[test]
fn round_trips_exact_string_slot_binding_and_node_count_boundaries() {
    let exact_key = "🙂".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES / 4);
    let exact_session = "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4);
    let mut repeated = document_with_slots(LAYOUT_DOCUMENT_MAX_SLOTS);
    repeated["root"]["leaf_key"] = Value::from(exact_key.as_str());
    repeated["focused_leaf_key"] = Value::from(exact_key.as_str());
    repeated["root"]["slot_keys"][0] = Value::from(exact_key.as_str());
    repeated["root"]["selected_slot_key"] = Value::from(exact_key.as_str());
    repeated["bindings"][0]["slot_key"] = Value::from(exact_key.as_str());
    repeated["bindings"][0]["session_id"] = Value::from(exact_session.as_str());
    let exact_repeated = typed(repeated);
    let wire = layout_document_to_proto(&exact_repeated).unwrap();
    assert_eq!(layout_document_from_proto(&wire).unwrap(), exact_repeated);

    let exact_nodes = exact_node_document();
    let wire = layout_document_to_proto(&exact_nodes).unwrap();
    assert_eq!(layout_document_from_proto(&wire).unwrap(), exact_nodes);
}

#[test]
fn refuses_a_deep_protobuf_before_recursive_conversion() {
    let reason = refusal(&proto_depth_document(LAYOUT_DOCUMENT_MAX_DEPTH + 1));
    assert!(
        reason.contains(&format!(
            "layout document exceeds depth {LAYOUT_DOCUMENT_MAX_DEPTH}"
        )),
        "{reason}"
    );
    let mut too_deep = depth_document(LAYOUT_DOCUMENT_MAX_DEPTH);
    too_deep.root = roost_protocol::layout::document::LayoutDocumentNode::Split(
        serde_json::from_value(split(
            leaf("side-extra", &[], None),
            serde_json::to_value(&too_deep.root).unwrap(),
        ))
        .unwrap(),
    );
    assert!(layout_document_to_proto(&too_deep).is_err());
}

#[test]
fn refuses_over_limit_protobuf_strings_and_repeated_fields() {
    let mut over_key = proto_depth_document(1);
    over_key.focused_leaf_key = "x".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES + 1);
    assert!(refusal(&over_key).contains("layout key"));

    let mut over_session = layout_document_to_proto(&typed(document_with_slots(1))).unwrap();
    over_session.bindings[0].session_id = format!(
        "{}x",
        "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4)
    );
    assert!(refusal(&over_session).contains("layout session_id"));

    let mut over_slots =
        layout_document_to_proto(&typed(document_with_slots(LAYOUT_DOCUMENT_MAX_SLOTS))).unwrap();
    root_leaf(&mut over_slots)
        .slot_keys
        .push("slot-extra".to_owned());
    assert!(refusal(&over_slots).contains("slots"));

    let mut over_nodes = layout_document_to_proto(&exact_node_document()).unwrap();
    let full = over_nodes.root.take().unwrap();
    over_nodes.root = MessageField::some(split_node(
        row(),
        0.5,
        Some(full),
        Some(leaf_node("extra-leaf", &[], None)),
    ));
    assert!(refusal(&over_nodes).contains("nodes"));

    let mut over_bindings = proto_depth_document(1);
    over_bindings.bindings = (0..=LAYOUT_DOCUMENT_MAX_BINDINGS)
        .map(|index| binding(&format!("slot-{index}"), &format!("session-{index}")))
        .collect();
    assert!(refusal(&over_bindings).contains("bindings"));
}
