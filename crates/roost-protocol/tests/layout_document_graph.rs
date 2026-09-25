//! The layout document's graph integrity: the references a saved document
//! makes, and the ones it is not allowed to make.
//!
//! A document that names a leaf or a slot it does not contain, repeats a key,
//! selects a slot its own leaf does not hold, or puts one session in two slots
//! is refused rather than half-applied. The size and depth bounds that decide
//! whether a document is walked at all are the other half, in
//! `layout_document.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;

use roost_protocol::layout::document::parse_layout_document_v1;

use support::{SESSION_A, SESSION_B, document, leaf, split};

#[test]
fn a_document_naming_a_leaf_or_a_slot_it_does_not_contain_is_refused() {
    let missing_focus = document(leaf("leaf-a", &[], None), "leaf-missing", json!([]));
    assert!(parse_layout_document_v1(&missing_focus).is_err());

    let missing_slot = document(
        leaf("leaf-a", &["slot-a"], Some("slot-missing")),
        "leaf-a",
        json!([]),
    );
    assert!(parse_layout_document_v1(&missing_slot).is_err());

    // Two slots, one binding: the leaf that holds slot-b is the one that has
    // to be repaired, so the refusal names that leaf.
    let unbound_slot = document(
        leaf("leaf-a", &["slot-a", "slot-b"], Some("slot-a")),
        "leaf-a",
        json!([{ "slot_key": "slot-a", "session_id": SESSION_A }]),
    );
    assert!(parse_layout_document_v1(&unbound_slot).is_err());

    let extra_binding = document(
        leaf("leaf-a", &["slot-a"], Some("slot-a")),
        "leaf-a",
        json!([
            { "slot_key": "slot-a", "session_id": SESSION_A },
            { "slot_key": "slot-extra", "session_id": SESSION_B },
        ]),
    );
    assert!(parse_layout_document_v1(&extra_binding).is_err());
}

#[test]
fn a_repeated_leaf_key_or_slot_key_is_refused() {
    let repeated_leaf = document(
        split(
            leaf("leaf-a", &["slot-a"], Some("slot-a")),
            leaf("leaf-a", &["slot-b"], Some("slot-b")),
        ),
        "leaf-a",
        json!([
            { "slot_key": "slot-a", "session_id": SESSION_A },
            { "slot_key": "slot-b", "session_id": SESSION_B },
        ]),
    );
    assert!(parse_layout_document_v1(&repeated_leaf).is_err());

    let repeated_slot = document(
        split(
            leaf("leaf-a", &["slot-a"], Some("slot-a")),
            leaf("leaf-b", &["slot-a"], Some("slot-a")),
        ),
        "leaf-a",
        json!([
            { "slot_key": "slot-a", "session_id": SESSION_A },
            { "slot_key": "slot-a", "session_id": SESSION_B },
        ]),
    );
    assert!(parse_layout_document_v1(&repeated_slot).is_err());
}

#[test]
fn a_selected_slot_must_belong_to_its_own_leaf() {
    let foreign_slot = document(
        split(
            leaf("leaf-a", &["slot-a"], Some("slot-b")),
            leaf("leaf-b", &["slot-b"], Some("slot-b")),
        ),
        "leaf-a",
        json!([
            { "slot_key": "slot-a", "session_id": SESSION_A },
            { "slot_key": "slot-b", "session_id": SESSION_B },
        ]),
    );
    assert!(parse_layout_document_v1(&foreign_slot).is_err());
}

#[test]
fn a_leaf_selects_a_slot_exactly_when_it_has_one() {
    let selection_without_slots =
        document(leaf("leaf-a", &[], Some("slot-b")), "leaf-a", json!([]));
    assert!(parse_layout_document_v1(&selection_without_slots).is_err());

    let selection_with_slots = document(leaf("leaf-a", &["slot-a"], None), "leaf-a", json!([]));
    assert!(parse_layout_document_v1(&selection_with_slots).is_err());

    let unbound = document(
        leaf("leaf-a", &["slot-a"], Some("slot-a")),
        "leaf-a",
        json!([]),
    );
    assert!(
        parse_layout_document_v1(&unbound).is_err(),
        "every slot must have exactly one binding"
    );
}

#[test]
fn one_session_may_not_occupy_two_slots() {
    let shared = document(
        leaf("leaf-a", &["slot-a", "slot-b"], Some("slot-a")),
        "leaf-a",
        json!([
            { "slot_key": "slot-a", "session_id": SESSION_A },
            { "slot_key": "slot-b", "session_id": SESSION_A },
        ]),
    );
    assert!(parse_layout_document_v1(&shared).is_err());
}

#[test]
fn the_document_names_sessions_without_proving_them_live() {
    // A saved document outlives the sessions it was exported against, so the
    // document layer checks that a binding names a slot, never that the session
    // still exists. The importing folder is what compares against its live set.
    let saved_against_a_long_dead_session = document(
        leaf("leaf-a", &["slot-a"], Some("slot-a")),
        "leaf-a",
        json!([{ "slot_key": "slot-a", "session_id": "not-a-uuid" }]),
    );
    assert!(parse_layout_document_v1(&saved_against_a_long_dead_session).is_ok());
}
