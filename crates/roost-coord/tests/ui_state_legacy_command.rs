//! The legacy UI commands and the layout document's canonical copy: which
//! sessions each command names, which wire values are refused before a database
//! is touched, and what a document looks like after it has been through the
//! shared parser.
//!
//! The legacy command path is reachable in v3 and is NOT dropped: `UiCommand`
//! still carries all nine oneof arms in the wire contract, eight of them are
//! fire-and-forget, and `UiDispatch` still accepts them. What v2 called "legacy"
//! is a naming choice about the acknowledged apply, not a retired shape.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use connectrpc::ErrorCode;
use roost_coord::ui_state::layout_proto::{canonical_layout_document, layout_document_from_proto};
use roost_coord::ui_state::legacy_command::{
    canonical_legacy_ui_command, legacy_ui_command_session_ids,
};
use roost_proto as proto;
use roost_proto::__buffa::oneof::layout_document_node::Node;
use roost_proto::__buffa::oneof::ui_command::Command;
use roost_proto::buffa::MessageField;

fn select_tab(session_id: &str) -> proto::UiCommand {
    proto::UiCommand {
        command: Some(Command::SelectTab(Box::new(proto::UiSelectTab {
            session_id: session_id.to_owned(),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

fn place_split(session_id: &str, anchor: &str, dir: &str) -> proto::UiCommand {
    proto::UiCommand {
        command: Some(Command::PlaceSplit(Box::new(proto::UiPlaceSplit {
            session_id: session_id.to_owned(),
            anchor_session_id: anchor.to_owned(),
            dir: dir.to_owned(),
            insert_first: true,
            ..Default::default()
        }))),
        ..Default::default()
    }
}

fn leaf_document(session_id: &str) -> proto::LayoutDocumentV1 {
    proto::LayoutDocumentV1 {
        schema_version: 1,
        root: MessageField::some(proto::LayoutDocumentNode {
            node: Some(Node::Leaf(Box::new(proto::LayoutDocumentLeaf {
                leaf_key: "leaf-1".to_owned(),
                slot_keys: vec!["slot-1".to_owned()],
                selected_slot_key: Some("slot-1".to_owned()),
                ..Default::default()
            }))),
            ..Default::default()
        }),
        focused_leaf_key: "leaf-1".to_owned(),
        bindings: vec![proto::LayoutDocumentBinding {
            slot_key: "slot-1".to_owned(),
            session_id: session_id.to_owned(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn each_command_names_exactly_the_sessions_it_acts_on() {
    assert_eq!(
        legacy_ui_command_session_ids(&select_tab("session-a")).expect("a selectTab names one"),
        vec!["session-a".to_owned()]
    );
    assert_eq!(
        legacy_ui_command_session_ids(&place_split("session-a", "session-b", "row"))
            .expect("a placeSplit names two"),
        vec!["session-a".to_owned(), "session-b".to_owned()]
    );
    let navigate = proto::UiCommand {
        command: Some(Command::Navigate(Box::new(proto::UiNavigate {
            path: "/s/session-a".to_owned(),
            ..Default::default()
        }))),
        ..Default::default()
    };
    assert!(
        legacy_ui_command_session_ids(&navigate)
            .expect("a navigate names none")
            .is_empty(),
        "a navigate names no session, so no database check is owed"
    );
}

#[test]
fn an_oversized_session_id_is_refused_before_any_lookup() {
    let oversized =
        "x".repeat(roost_protocol::layout::LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES + 1);
    let refused = legacy_ui_command_session_ids(&select_tab(&oversized))
        .expect_err("an over-long session id is a malformed argument");
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
}

#[test]
fn apply_layout_is_not_canonicalisable_as_a_legacy_command() {
    let apply = proto::UiCommand {
        command: Some(Command::ApplyLayout(Box::new(proto::UiApplyLayout {
            document: MessageField::some(leaf_document("session-a")),
            ..Default::default()
        }))),
        ..Default::default()
    };
    assert!(
        legacy_ui_command_session_ids(&apply)
            .expect("an apply names no session")
            .is_empty(),
        "an apply's sessions are checked through its document instead"
    );
    let refused = canonical_legacy_ui_command(&apply)
        .expect_err("an apply is admitted only by UiApplyLayout");
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert!(
        canonical_legacy_ui_command(&proto::UiCommand::default())
            .expect_err("a command with no case is refused")
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("unsupported UI command")
    );
}

#[test]
fn a_split_direction_and_an_arrange_preset_are_the_two_string_enums() {
    assert!(
        canonical_legacy_ui_command(&place_split("session-a", "session-b", "diagonal"))
            .expect_err("a split direction is one of two words")
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("split direction")
    );
    let unknown_preset = proto::UiCommand {
        command: Some(Command::Arrange(Box::new(proto::UiArrange {
            preset: "spiral".to_owned(),
            ..Default::default()
        }))),
        ..Default::default()
    };
    assert!(
        canonical_legacy_ui_command(&unknown_preset)
            .expect_err("an arrange preset is one of five words")
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("arrange preset")
    );
    for preset in ["even", "rows", "tiled", "main-vertical", "balance"] {
        let command = proto::UiCommand {
            command: Some(Command::Arrange(Box::new(proto::UiArrange {
                preset: preset.to_owned(),
                ..Default::default()
            }))),
            ..Default::default()
        };
        assert!(
            canonical_legacy_ui_command(&command).is_ok(),
            "{preset} is a preset the browser renders"
        );
    }
}

#[test]
fn a_canonical_command_carries_only_its_own_fields() {
    let canonical = canonical_legacy_ui_command(&place_split("session-a", "session-b", "col"))
        .expect("a well-formed split is canonicalisable");
    let Some(Command::PlaceSplit(split)) = canonical.command.as_ref() else {
        panic!("the canonical command is still a placeSplit");
    };
    assert_eq!(split.dir, "col");
    assert!(split.insert_first);
    assert_eq!(split.session_id, "session-a");
    assert_eq!(split.anchor_session_id, "session-b");
}

#[test]
fn a_document_survives_the_shared_parser_and_keeps_its_bindings() {
    let canonical = canonical_layout_document(&leaf_document("session-a"))
        .expect("a well-formed document is admitted");
    assert_eq!(canonical.schema_version, 1);
    assert_eq!(canonical.focused_leaf_key, "leaf-1");
    assert_eq!(canonical.bindings[0].slot_key, "slot-1");
    assert_eq!(canonical.bindings[0].session_id, "session-a");

    let parsed = layout_document_from_proto(&canonical).expect("the canonical copy parses back");
    assert_eq!(parsed.schema_version, 1);
    assert_eq!(parsed.bindings[0].session_id, "session-a");
    let roost_protocol::layout::LayoutDocumentNode::Leaf(leaf) = &parsed.root else {
        panic!("the root is still the leaf it was");
    };
    assert_eq!(leaf.slot_keys, vec!["slot-1".to_owned()]);
}

#[test]
fn a_document_with_a_non_portable_shape_is_refused_rather_than_guessed_at() {
    let mut future_version = leaf_document("session-a");
    future_version.schema_version = 2;
    assert!(
        canonical_layout_document(&future_version).is_err(),
        "a schema version this build does not implement is not a document to render"
    );

    let mut no_root = leaf_document("session-a");
    no_root.root = MessageField::none();
    assert!(canonical_layout_document(&no_root).is_err());

    let mut oversized_key = leaf_document("session-a");
    oversized_key.focused_leaf_key =
        "x".repeat(roost_protocol::layout::LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES + 1);
    assert!(
        canonical_layout_document(&oversized_key).is_err(),
        "the key bound is checked before the graph is walked"
    );
}
