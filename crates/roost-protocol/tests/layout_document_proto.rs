//! The layout document's protobuf adapter: the recursive mapping both ways,
//! the null-selection encoding, every malformed message it must refuse, and
//! the iterative resource preflight that runs before any recursion.
//!
//! Ports `packages/protocol/tests/layout-document-proto.test.ts` and the
//! protobuf half of `layout-document-bounds.test.ts`. The bounds test's cyclic
//! message has no Rust form: a protobuf tree here is owned, so a node cannot
//! be its own child.
#![allow(clippy::unwrap_used, clippy::expect_used)]

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

fn typed(value: Value) -> LayoutDocumentV1 {
    parse_layout_document_v1(&value).unwrap()
}

fn nested_document() -> LayoutDocumentV1 {
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

fn leaf_node(leaf_key: &str, slot_keys: &[&str], selected: Option<&str>) -> proto::LayoutDocumentNode {
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

fn split_node(
    direction: EnumValue<proto::LayoutDirection>,
    ratio: f64,
    first: Option<proto::LayoutDocumentNode>,
    second: Option<proto::LayoutDocumentNode>,
) -> proto::LayoutDocumentNode {
    let field = |node: Option<proto::LayoutDocumentNode>| node.map_or(MessageField::none(), MessageField::some);
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

fn binding(slot_key: &str, session_id: &str) -> proto::LayoutDocumentBinding {
    proto::LayoutDocumentBinding {
        slot_key: slot_key.to_owned(),
        session_id: session_id.to_owned(),
        ..Default::default()
    }
}

fn occupied_leaf_proto() -> proto::LayoutDocumentV1 {
    proto::LayoutDocumentV1 {
        schema_version: 1,
        root: MessageField::some(leaf_node("leaf-a", &["slot-a"], Some("slot-a"))),
        focused_leaf_key: "leaf-a".to_owned(),
        bindings: vec![binding("slot-a", SESSION_A)],
        ..Default::default()
    }
}

fn empty_split_proto(direction: EnumValue<proto::LayoutDirection>, ratio: f64) -> proto::LayoutDocumentV1 {
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

fn root_leaf(document: &mut proto::LayoutDocumentV1) -> &mut proto::LayoutDocumentLeaf {
    match document.root.as_option_mut().and_then(|root| root.node.as_mut()) {
        Some(Node::Leaf(leaf)) => leaf,
        _ => panic!("expected a root leaf"),
    }
}

fn root_split(document: &mut proto::LayoutDocumentV1) -> &mut proto::LayoutDocumentSplit {
    match document.root.as_option_mut().and_then(|root| root.node.as_mut()) {
        Some(Node::Split(split)) => split,
        _ => panic!("expected a root split"),
    }
}

fn assert_refused(document: &proto::LayoutDocumentV1) {
    assert!(layout_document_from_proto(document).is_err(), "{document:?} was accepted");
}

fn refusal(document: &proto::LayoutDocumentV1) -> String {
    layout_document_from_proto(document).unwrap_err().to_string()
}

fn row() -> EnumValue<proto::LayoutDirection> {
    proto::LayoutDirection::Row.into()
}

fn depth_document(depth: usize) -> LayoutDocumentV1 {
    let mut root = leaf("deep-leaf", &[], None);
    for level in 2..=depth {
        root = split(leaf(&format!("side-{level}"), &[], None), root);
    }
    parse_layout_document_v1(&document(root, "deep-leaf", json!([]))).unwrap()
}

fn proto_depth_document(depth: usize) -> proto::LayoutDocumentV1 {
    let mut root = leaf_node("deep-leaf", &[], None);
    for level in 2..=depth {
        root = split_node(row(), 0.5, Some(leaf_node(&format!("side-{level}"), &[], None)), Some(root));
    }
    proto::LayoutDocumentV1 {
        schema_version: 1,
        root: MessageField::some(root),
        focused_leaf_key: "deep-leaf".to_owned(),
        ..Default::default()
    }
}

fn complete_tree(depth: u32, next_key: &mut usize) -> Value {
    if depth == 1 {
        let key = format!("leaf-{next_key}");
        *next_key += 1;
        return leaf(&key, &[], None);
    }
    let first = complete_tree(depth - 1, next_key);
    split(first, complete_tree(depth - 1, next_key))
}

fn exact_node_document() -> LayoutDocumentV1 {
    let complete_depth = (LAYOUT_DOCUMENT_MAX_NODES + 1).ilog2();
    typed(document(complete_tree(complete_depth, &mut 0), "leaf-0", json!([])))
}

fn document_with_slots(slot_count: usize) -> Value {
    let slot_keys: Vec<String> = (0..slot_count).map(|index| format!("slot-{index}")).collect();
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

#[test]
fn round_trips_nested_trees_empty_leaves_null_selection_and_ratio_endpoints() {
    let source = nested_document();
    let mut wire = layout_document_to_proto(&source).unwrap();
    assert_eq!(layout_document_from_proto(&wire).unwrap(), source);

    let split = root_split(&mut wire);
    assert_eq!(split.direction, proto::LayoutDirection::Row);
    assert_eq!(split.ratio, LAYOUT_RATIO_MIN);
    let Some(Node::Split(nested)) = split.second.as_option().and_then(|node| node.node.as_ref()) else {
        panic!("expected a nested split");
    };
    assert_eq!(nested.direction, proto::LayoutDirection::Col);
    assert_eq!(nested.ratio, LAYOUT_RATIO_MAX);
    let Some(Node::Leaf(empty)) = nested.first.as_option().and_then(|node| node.node.as_ref()) else {
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
    assert_refused(&empty_split_proto(proto::LayoutDirection::Unspecified.into(), 0.5));
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
        reason.contains(&format!("layout document exceeds depth {LAYOUT_DOCUMENT_MAX_DEPTH}")),
        "{reason}"
    );
    let mut too_deep = depth_document(LAYOUT_DOCUMENT_MAX_DEPTH);
    too_deep.root = roost_protocol::layout::document::LayoutDocumentNode::Split(
        serde_json::from_value(split(leaf("side-extra", &[], None), serde_json::to_value(&too_deep.root).unwrap()))
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
    over_session.bindings[0].session_id =
        format!("{}x", "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4));
    assert!(refusal(&over_session).contains("layout session_id"));

    let mut over_slots =
        layout_document_to_proto(&typed(document_with_slots(LAYOUT_DOCUMENT_MAX_SLOTS))).unwrap();
    root_leaf(&mut over_slots).slot_keys.push("slot-extra".to_owned());
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
