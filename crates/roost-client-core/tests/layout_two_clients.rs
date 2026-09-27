//! What two clients reading one document do: the arrangement a re-fetch and the
//! arrangement a broadcast must agree on, and a folder's first arrangement.
//!
//! Split from `layout_document_apply.rs` because these are the two-client
//! properties, and the file they came from is the single client's document
//! rules. Both are the reason there is exactly one function that turns a
//! document into a committed arrangement.

mod layout_support;

use roost_client_core::client::ui_state::{
    LayoutApplyCommand, LayoutApplyConsumption, LayoutApplyExecution, ReportedTab,
    compose_layout_apply, execute_targeted_layout_apply,
};
use roost_client_core::store::layout::{
    PaneLayout, PaneLeaf, PaneNode, PaneSplit, all_leaves, apply_layout_document, default_layout,
    export_layout_document, reorder_tab,
};
use roost_protocol::layout::LayoutDocumentV1;
use roost_protocol::layout::document::LayoutDirection;

use layout_support::{CountedIds, RecordingHost, ok, session_ids, single_pane_document};

const ALPHA: &str = "alpha";
const BETA: &str = "beta";
const GAMMA: &str = "gamma";
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
fn the_re_fetched_and_the_broadcast_arrangement_agree_after_a_reordering() {
    // One client reorders its own strip and publishes what it now holds.
    let original = two_panes();
    let reordered = reorder_tab(&original, "p1", session_ids(&[BETA, ALPHA]));
    let document = ok(
        export_layout_document(FOLDER, &three_sessions(), &reordered),
        "export",
    );
    // The reordering is real, so agreement below is not agreement on the
    // arrangement nobody changed.
    assert_ne!(shape(&reordered), shape(&original));

    // A second client applies it as a broadcast command.
    let mut broadcast = RecordingHost::new(
        TAB,
        SOCKET,
        RecordingHost::folder(FOLDER, ALPHA, &[ALPHA, BETA, GAMMA]),
    );
    let execution = execute_targeted_layout_apply(Some(&command(document.clone())), &mut broadcast);
    assert!(matches!(
        execution,
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Applied { .. })
    ));

    // A third client re-reads the same reported document, because the tab it
    // was composed against has moved.
    let mut refetch = RecordingHost::new(
        OTHER_TAB,
        OTHER_SOCKET,
        RecordingHost::folder(FOLDER, ALPHA, &[ALPHA, BETA, GAMMA]),
    );
    let reported = ReportedTab {
        fingerprint: "fp-1".to_owned(),
        tab_id: TAB.to_owned(),
        layout_document: Some(document.clone()),
    };
    ok(
        apply_layout_document(
            &mut refetch.records,
            FOLDER,
            ok(reported.layout_document.as_ref(), "a reported document"),
            &three_sessions(),
            &mut refetch.ids,
        ),
        "re-fetch apply",
    );

    let broadcast_layout = ok(
        broadcast.records.stored(FOLDER),
        "the broadcast arrangement",
    );
    let refetched = ok(refetch.records.stored(FOLDER), "the re-fetched arrangement");
    // One source, one answer: the two paths are the same function, and this is
    // what pins that rather than leaving it to a reader's trust.
    assert_eq!(shape(broadcast_layout), shape(refetched));
    assert_eq!(shape(refetched), shape(&reordered));
}

#[test]
fn a_folder_with_no_stored_layout_applies_its_first_arrangement() {
    let mut host = RecordingHost::new(
        TAB,
        SOCKET,
        RecordingHost::folder(FOLDER, ALPHA, &[ALPHA, BETA]),
    );
    assert!(host.records.is_empty());

    // The first arrangement is not a stale one. Nothing about a missing prior
    // layout may be read as a fence, or a new folder could never be tiled.
    let execution = execute_targeted_layout_apply(
        Some(&command(single_pane_document(&[ALPHA, BETA], BETA))),
        &mut host,
    );
    assert_eq!(
        execution,
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Applied {
            selected_session_id: Some(BETA.to_owned()),
        })
    );
    assert_eq!(host.records.len(), 1);
    let stored = ok(host.records.stored(FOLDER), "the first arrangement");
    assert_eq!(
        shape(stored),
        vec![(session_ids(&[ALPHA, BETA]), BETA.to_owned(), true)]
    );

    // And the calling side composes against a tab that has reported no document
    // yet, which is the same first-arrangement case seen from the other end.
    let mut ids = CountedIds::new("caller");
    let layout = default_layout(&session_ids(&[ALPHA, BETA]), &mut ids);
    let reported = ReportedTab {
        fingerprint: "fp-1".to_owned(),
        tab_id: TAB.to_owned(),
        layout_document: None,
    };
    let composed = ok(
        compose_layout_apply(
            FOLDER,
            &session_ids(&[ALPHA, BETA]),
            &layout,
            &reported,
            SOCKET,
        ),
        "compose",
    );
    assert_eq!(composed.document.bindings.len(), 2);
    assert_eq!(composed.target.socket_id, SOCKET);
}
