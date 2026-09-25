//! The layout document's resource bounds: every limit pinned at its endpoint
//! and one entry past it.
//!
//! The preflight is the only thing standing between a hostile document and the
//! parse, so a bound that is never probed past its endpoint is a bound nothing
//! is holding. The references a saved document makes — leaves, slots, and the
//! sessions bound to them — are the other half, in `layout_document_graph.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_protocol::layout::document::{
    LAYOUT_RATIO_MAX, LAYOUT_RATIO_MIN, is_layout_document_v1, parse_layout_document_v1,
};
use roost_protocol::layout::preflight::{
    LAYOUT_DOCUMENT_MAX_BINDINGS, LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_NODES, LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_SLOTS,
};
use serde_json::{Value, json};

use support::{SESSION_A, document, leaf, split};

fn document_with_slots(slot_count: usize, binding_count: usize) -> Value {
    let slot_keys: Vec<String> = (0..slot_count)
        .map(|index| format!("slot-{index}"))
        .collect();
    let bindings: Vec<Value> = (0..binding_count)
        .map(|index| {
            let slot_key = slot_keys
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("extra-{index}"));
            json!({ "slot_key": slot_key, "session_id": format!("session-{index}") })
        })
        .collect();
    document(
        leaf(
            "leaf-slots",
            &slot_keys.iter().map(String::as_str).collect::<Vec<_>>(),
            slot_keys.first().map(String::as_str),
        ),
        "leaf-slots",
        Value::Array(bindings),
    )
}

fn document_of_depth(depth: usize) -> Value {
    let mut root = leaf("deep-leaf", &[], None);
    for level in 2..=depth {
        root = split(leaf(&format!("side-{level}"), &[], None), root);
    }
    document(root, "deep-leaf", json!([]))
}

fn complete_tree(depth: usize, next_key: &mut usize) -> Value {
    if depth == 1 {
        let leaf_key = format!("leaf-{}", *next_key);
        *next_key += 1;
        return leaf(&leaf_key, &[], None);
    }
    split(
        complete_tree(depth - 1, next_key),
        complete_tree(depth - 1, next_key),
    )
}

fn one_leaf_document() -> Value {
    document(
        leaf("leaf-a", &["slot-a"], Some("slot-a")),
        "leaf-a",
        json!([{ "slot_key": "slot-a", "session_id": SESSION_A }]),
    )
}

#[test]
fn a_one_leaf_document_round_trips_through_the_parse() {
    let source = one_leaf_document();
    let parsed = parse_layout_document_v1(&source).expect("a valid document parses");
    assert_eq!(
        serde_json::to_value(&parsed).expect("a document serializes"),
        source
    );
    assert!(is_layout_document_v1(&source));
}

#[test]
fn a_split_ratio_is_admitted_only_between_its_endpoints() {
    for ratio in [LAYOUT_RATIO_MIN, 0.5, LAYOUT_RATIO_MAX] {
        let candidate = document(
            split(leaf("leaf-a", &[], None), leaf("leaf-b", &[], None)),
            "leaf-a",
            json!([]),
        );
        let mut candidate = candidate;
        candidate["root"]["ratio"] = json!(ratio);
        assert!(
            parse_layout_document_v1(&candidate).is_ok(),
            "ratio {ratio} must be admitted"
        );
    }
    for ratio in [
        LAYOUT_RATIO_MIN - f64::EPSILON,
        LAYOUT_RATIO_MAX + f64::EPSILON,
        -1.0,
        2.0,
    ] {
        let mut candidate = document(
            split(leaf("leaf-a", &[], None), leaf("leaf-b", &[], None)),
            "leaf-a",
            json!([]),
        );
        candidate["root"]["ratio"] = json!(ratio);
        assert!(
            parse_layout_document_v1(&candidate).is_err(),
            "ratio {ratio} must be refused"
        );
    }
}

#[test]
fn an_unknown_key_anywhere_in_the_tree_is_refused() {
    for mutate in [
        (|value: &mut Value| value["unknown"] = json!(true)) as fn(&mut Value),
        |value: &mut Value| value["root"]["unknown"] = json!(true),
        |value: &mut Value| value["root"]["first"]["unknown"] = json!(true),
        |value: &mut Value| value["bindings"][0]["unknown"] = json!(true),
    ] {
        let mut candidate = document(
            split(
                leaf("leaf-a", &["slot-a"], Some("slot-a")),
                leaf("leaf-b", &[], None),
            ),
            "leaf-a",
            json!([{ "slot_key": "slot-a", "session_id": SESSION_A }]),
        );
        mutate(&mut candidate);
        assert!(parse_layout_document_v1(&candidate).is_err());
    }
}

#[test]
fn a_node_that_is_neither_a_leaf_nor_a_split_is_refused() {
    for root in [
        json!({ "kind": "branch", "leaf_key": "leaf-a", "slot_keys": [], "selected_slot_key": null }),
        json!({ "leaf_key": "leaf-a", "slot_keys": [], "selected_slot_key": null }),
        json!(null),
    ] {
        assert!(parse_layout_document_v1(&document(root, "leaf-a", json!([]))).is_err());
    }
}

#[test]
fn a_key_and_a_session_id_are_admitted_at_their_utf8_bounds() {
    let exact_key = "🙂".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES / 4);
    let exact_session = "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4);
    let at_bound = document(
        leaf(&exact_key, &[exact_key.as_str()], Some(&exact_key)),
        &exact_key,
        json!([{ "slot_key": exact_key, "session_id": exact_session }]),
    );
    assert!(parse_layout_document_v1(&at_bound).is_ok());

    let over_key = format!("{exact_key}x");
    let over_key = document(leaf(&over_key, &[], None), "leaf-a", json!([]));
    assert!(parse_layout_document_v1(&over_key).is_err());

    let over_session = document_with_slots(1, 1);
    let mut over_session = over_session;
    over_session["bindings"][0]["session_id"] = json!(format!("{exact_session}x"));
    assert!(!is_layout_document_v1(&over_session));
}

#[test]
fn a_repeated_subtree_is_bounded_rather_than_walked_forever() {
    // A document whose splits keep descending into the same shape: eight
    // doublings is 511 nodes, so the node cap refuses it, and a walk that
    // trusted its input instead of counting would have had to walk all of them.
    let mut repeated = leaf("leaf-a", &[], None);
    for _ in 0..8 {
        repeated = split(repeated.clone(), repeated.clone());
    }
    let hostile = document(repeated, "leaf-a", json!([]));
    assert!(!is_layout_document_v1(&hostile));
}

#[test]
fn depth_and_node_count_are_admitted_at_their_bounds() {
    assert!(parse_layout_document_v1(&document_of_depth(LAYOUT_DOCUMENT_MAX_DEPTH)).is_ok());
    assert!(parse_layout_document_v1(&document_of_depth(LAYOUT_DOCUMENT_MAX_DEPTH + 1)).is_err());

    // 255 nodes is a complete tree of eight levels, and one node too many.
    let exact_tree = complete_tree(8, &mut 0);
    assert_eq!(count_nodes(&exact_tree), LAYOUT_DOCUMENT_MAX_NODES);
    assert!(parse_layout_document_v1(&document(exact_tree.clone(), "leaf-0", json!([]))).is_ok());
    let over_nodes = document(
        split(exact_tree, leaf("extra-leaf", &[], None)),
        "leaf-0",
        json!([]),
    );
    assert!(parse_layout_document_v1(&over_nodes).is_err());
}

#[test]
fn an_extremely_deep_document_is_refused_without_recursing() {
    // 10_000 levels is past anything a stack survives if the preflight is
    // removed, which is exactly the regression this case exists to catch.
    let hostile = document_of_depth(10_000);
    let admitted = is_layout_document_v1(&hostile);
    // Leaked, not dropped: `serde_json::Value`'s `Drop` recurses once per level,
    // so releasing a tree this deep would abort the test thread before the
    // assertion above could report. The process exits moments later anyway.
    std::mem::forget(hostile);
    assert!(!admitted);
}

#[test]
fn slots_and_bindings_are_admitted_at_their_counts() {
    assert!(
        parse_layout_document_v1(&document_with_slots(
            LAYOUT_DOCUMENT_MAX_SLOTS,
            LAYOUT_DOCUMENT_MAX_SLOTS
        ))
        .is_ok()
    );

    let over_slots =
        document_with_slots(LAYOUT_DOCUMENT_MAX_SLOTS + 1, LAYOUT_DOCUMENT_MAX_BINDINGS);
    assert!(parse_layout_document_v1(&over_slots).is_err());

    let over_bindings =
        document_with_slots(LAYOUT_DOCUMENT_MAX_SLOTS, LAYOUT_DOCUMENT_MAX_BINDINGS + 1);
    assert!(parse_layout_document_v1(&over_bindings).is_err());
}

fn count_nodes(node: &Value) -> usize {
    match node.get("kind").and_then(Value::as_str) {
        Some("split") => 1 + count_nodes(&node["first"]) + count_nodes(&node["second"]),
        Some("leaf") => 1,
        _ => 0,
    }
}
