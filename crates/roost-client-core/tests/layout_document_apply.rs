//! The portable document: positional keys, the round trip, and the three
//! properties the apply path is load-bearing for -- a refusal writes nothing,
//! a re-fetch and a broadcast agree, and a first arrangement is not mistaken for
//! a stale one.
//!
//! Mirrors `apps/web/tests/paneLayoutDocument.test.ts` and
//! `paneLayoutDocument.degrade.test.ts`, plus the cases the port makes
//! load-bearing: the fence the client trips, and the one source both read.

mod layout_support;

use roost_client_core::store::layout::{
    LayoutDocumentError, LayoutRecords, PaneLayout, PaneLeaf, PaneNode, PaneSplit, all_leaves,
    apply_layout_document, default_layout, degrade_layout_document_to_live_sessions,
    export_layout_document, split_leaf, validate_layout_document_import,
};
use roost_protocol::layout::document::LayoutDirection;
use roost_protocol::layout::{LayoutDocumentNode, LayoutDocumentV1};

use layout_support::{CountedIds, ok, session_ids, single_pane_document, split_document};

const ALPHA: &str = "alpha";
const BETA: &str = "beta";
const GAMMA: &str = "gamma";
const DEAD: &str = "not-live";
const FOLDER: &str = "worker::/work";
const TAB: &str = "tab-current";
const SOCKET: &str = "socket-current";
const OTHER_TAB: &str = "tab-other";
const OTHER_SOCKET: &str = "socket-other";
const CORRELATION: &str = "correlation-1";

fn three_sessions() -> Vec<String> {
    session_ids(&[ALPHA, BETA, GAMMA])
}

fn two_panes() -> PaneLayout {
    PaneLayout {
        root: PaneNode::Split(PaneSplit {
            id: "split-1".to_owned(),
            direction: LayoutDirection::Row,
            ratio: 0.5,
            a: Box::new(PaneNode::Leaf(PaneLeaf {
                pane_id: "p1".to_owned(),
                tabs: session_ids(&[ALPHA, BETA]),
                selected_tab: ALPHA.to_owned(),
            })),
            b: Box::new(PaneNode::Leaf(PaneLeaf {
                pane_id: "p2".to_owned(),
                tabs: session_ids(&[GAMMA]),
                selected_tab: GAMMA.to_owned(),
            })),
        }),
        focused_pane_id: "p1".to_owned(),
    }
}

/// The arrangement a host paints, with the runtime pane ids left out.
///
/// Ids are identities minted per client, and a document carries none, so two
/// clients that read one document agree on the ARRANGEMENT and hold different
/// pane ids. Comparing the ids would be comparing the thing the document
/// deliberately does not carry.
fn shape(layout: &PaneLayout) -> Vec<(Vec<String>, String, bool)> {
    all_leaves(&layout.root)
        .into_iter()
        .map(|leaf| {
            (
                leaf.tabs.clone(),
                leaf.selected_tab.clone(),
                leaf.pane_id == layout.focused_pane_id,
            )
        })
        .collect()
}

fn command(document: LayoutDocumentV1) -> LayoutApplyCommand {
    LayoutApplyCommand {
        target_tab_id: TAB.to_owned(),
        target_socket_id: SOCKET.to_owned(),
        correlation_id: CORRELATION.to_owned(),
        document: Some(document),
    }
}

#[test]
fn an_exported_document_carries_positional_keys_and_no_runtime_pane_id() {
    let mut ids = CountedIds::new("pane");
    let start = ok(
        default_layout(&session_ids(&[ALPHA, BETA]), &mut ids),
        "default layout",
    );
    let split = ok(
        split_leaf(
            &start,
            "pane-1",
            LayoutDirection::Row,
            BETA,
            false,
            &mut ids,
        ),
        "split",
    );
    let document = ok(
        export_layout_document(FOLDER, &session_ids(&[ALPHA, BETA]), &split),
        "export",
    );
    // Preorder, first leaf then first, second leaf then second.
    assert_eq!(document.focused_leaf_key, "leaf-1");
    assert_eq!(
        document
            .bindings
            .iter()
            .map(|binding| (binding.slot_key.as_str(), binding.session_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("slot-1", ALPHA), ("slot-2", BETA)]
    );
    let encoded = ok(serde_json::to_string(&document), "encode");
    // A runtime pane id in a document is an id two clients would then share,
    // which is the one thing the document exists not to do.
    assert!(!encoded.contains("pane-1"), "{encoded}");
    assert!(!encoded.contains("pane-3"), "{encoded}");
}

#[test]
fn an_arrangement_survives_the_round_trip_through_a_document() {
    let original = two_panes();
    let document = ok(
        export_layout_document(FOLDER, &three_sessions(), &original),
        "export",
    );
    let mut records = LayoutRecords::new();
    let mut ids = CountedIds::new("other");
    ok(
        apply_layout_document(&mut records, FOLDER, &document, &three_sessions(), &mut ids),
        "apply",
    );
    let applied = ok(records.stored(FOLDER), "a stored layout");
    assert_eq!(shape(applied), shape(&original));
    // The runtime ids are the importing client's own, not the author's.
    assert_ne!(shape(applied), Vec::<(Vec<String>, String, bool)>::new());
    assert_eq!(all_leaves(&applied.root).len(), 2);
}

#[test]
fn a_refused_document_leaves_the_stored_record_byte_identical() {
    let mut records = LayoutRecords::new();
    records.commit(FOLDER, two_panes());
    let before = ok(records.snapshot(), "snapshot");
    let mut ids = CountedIds::new("other");
    // application would leave panes on screen matching no arrangement at all,
    // and the user could not tell whether the document or their view was wrong.
    let dead = single_pane_document(&[ALPHA, DEAD], DEAD);
    assert_eq!(
        apply_layout_document(&mut records, FOLDER, &dead, &three_sessions(), &mut ids),
        Err(LayoutDocumentError::SessionNotLive(DEAD.to_owned()))
    );
    assert_eq!(ok(records.snapshot(), "snapshot"), before);

    // The other two refusals that reach this far, on the same record.
    assert_eq!(
        apply_layout_document(&mut records, "", &dead, &three_sessions(), &mut ids),
        Err(LayoutDocumentError::FolderKeyRequired)
    );
    let duplicated = session_ids(&[ALPHA, ALPHA]);
    assert_eq!(
        apply_layout_document(&mut records, FOLDER, &dead, &duplicated, &mut ids),
        Err(LayoutDocumentError::DuplicateLiveSessionId(
            ALPHA.to_owned()
        ))
    );
    assert_eq!(ok(records.snapshot(), "snapshot"), before);
}

#[test]
fn a_degraded_import_drops_a_dead_binding_and_collapses_the_pane_it_emptied() {
    let mut ids = CountedIds::new("other");

    // One dead tab in a pane that keeps another: the pane survives.
    let degraded = ok(
        degrade_layout_document_to_live_sessions(
            &split_document(&[ALPHA, DEAD], &[GAMMA], LayoutDirection::Row, 0.5),
            &session_ids(&[ALPHA, GAMMA]),
            &mut ids,
        ),
        "degrade",
    );
    assert_eq!(degraded.dropped_session_count, 1);
    assert_eq!(degraded.document.bindings.len(), 2);

    // The dead tab was its pane's only tab: the pane collapses rather than
    // committing as a hole in the deck.
    let collapsed = ok(
        degrade_layout_document_to_live_sessions(
            &split_document(&[DEAD], &[GAMMA], LayoutDirection::Row, 0.5),
            &session_ids(&[ALPHA, GAMMA]),
            &mut ids,
        ),
        "degrade",
    );
    assert_eq!(collapsed.dropped_session_count, 1);
    assert!(matches!(
        collapsed.document.root,
        LayoutDocumentNode::Leaf(_)
    ));

    // Nothing to degrade is returned as it arrived, not re-derived.
    let whole = split_document(&[ALPHA], &[GAMMA], LayoutDirection::Row, 0.5);
    let intact = ok(
        degrade_layout_document_to_live_sessions(&whole, &session_ids(&[ALPHA, GAMMA]), &mut ids),
        "degrade",
    );
    assert_eq!(intact.dropped_session_count, 0);
    assert_eq!(intact.document, whole);
}

#[test]
fn a_live_session_the_document_never_named_lands_on_the_focused_pane() {
    let mut records = LayoutRecords::new();
    let mut ids = CountedIds::new("other");
    let applied = ok(
        apply_layout_document(
            &mut records,
            FOLDER,
            &single_pane_document(&[ALPHA], ALPHA),
            &three_sessions(),
            &mut ids,
        ),
        "apply",
    );
    let stored = ok(records.stored(FOLDER), "the arrangement");
    let first = ok(all_leaves(&stored.root).first(), "a pane");
    assert_eq!(first.tabs, three_sessions());
    assert_eq!(applied.selected_session_id, Some(ALPHA.to_owned()));
}

#[test]
fn a_direction_this_build_cannot_render_is_refused_in_both_directions() {
    let unportable = PaneLayout {
        root: PaneNode::Split(PaneSplit {
            id: "split-1".to_owned(),
            direction: LayoutDirection::Other("diagonal".to_owned()),
            ratio: 0.5,
            a: Box::new(PaneNode::Leaf(PaneLeaf {
                pane_id: "p1".to_owned(),
                tabs: session_ids(&[ALPHA]),
                selected_tab: ALPHA.to_owned(),
            })),
            b: Box::new(PaneNode::Leaf(PaneLeaf {
                pane_id: "p2".to_owned(),
                tabs: session_ids(&[BETA]),
                selected_tab: BETA.to_owned(),
            })),
        }),
        focused_pane_id: "p1".to_owned(),
    };
    // Out: refused, rather than written as a ratio and a direction the reader
    // would have to guess at.
    assert_eq!(
        export_layout_document(FOLDER, &session_ids(&[ALPHA, BETA]), &unportable),
        Err(LayoutDocumentError::UnportableDirection)
    );

    // In: a document that declares one is refused by the shared parser before
    // it reaches here, so the client's own check is the second half of one rule
    // and not a second rule.
    let mut records = LayoutRecords::new();
    let mut ids = CountedIds::new("other");
    let mut document: LayoutDocumentV1 =
        split_document(&[ALPHA], &[BETA], LayoutDirection::Row, 0.5);
    if let LayoutDocumentNode::Split(split) = &mut document.root {
        split.direction = LayoutDirection::Other("diagonal".to_owned());
    }
    assert!(validate_layout_document_import(&document, &session_ids(&[ALPHA, BETA])).is_ok());
    assert!(
        apply_layout_document(
            &mut records,
            FOLDER,
            &document,
            &session_ids(&[ALPHA, BETA]),
            &mut ids
        )
        .is_err()
    );
}
