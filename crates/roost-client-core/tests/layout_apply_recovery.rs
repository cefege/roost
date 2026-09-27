//! The fence on the CALLING side: an apply composed against a tab, and what the
//! client owes when the answer says that tab has moved.
//!
//! The load-bearing property is a TYPE, not a rule: `LayoutApplyRecovery` has
//! no arm that carries a document forward, so re-sending an arrangement against
//! a fence is a state this client cannot reach. These tests pin that, and the
//! cases around it -- a refusal, an answer on the wrong socket, an answer for an
//! apply never issued, and a second apply to a tab that has not answered the
//! first.

mod layout_support;

use roost_client_core::client::ui_state::{
    LAYOUT_APPLY_REFETCH_REASON, LayoutApplyAnswer, LayoutApplyRecovery, PendingLayoutApplies,
    ReportedTab, compose_layout_apply, settle_layout_apply,
};
use roost_client_core::store::layout::{
    LayoutDocumentError, PaneLayout, default_layout, split_leaf,
};
use roost_protocol::layout::document::LayoutDirection;

use layout_support::{CountedIds, ok, session_ids, single_pane_document};

const ALPHA: &str = "alpha";
const BETA: &str = "beta";
const GAMMA: &str = "gamma";
const FOLDER: &str = "worker::/work";
const TAB: &str = "tab-other";
const SOCKET: &str = "socket-current";
const MOVED_SOCKET: &str = "socket-moved";

/// A two-pane arrangement, which is what a caller composes from.
fn arranged() -> PaneLayout {
    let mut ids = CountedIds::new("caller");
    let start = default_layout(&session_ids(&[ALPHA, BETA]), &mut ids);
    split_leaf(
        &start,
        "caller-1",
        LayoutDirection::Row,
        BETA,
        false,
        &mut ids,
    )
}

fn reported() -> ReportedTab {
    ReportedTab {
        fingerprint: "fp-1".to_owned(),
        tab_id: TAB.to_owned(),
        layout_document: Some(single_pane_document(&[ALPHA, BETA], ALPHA)),
    }
}

fn composed() -> roost_client_core::client::ui_state::PendingLayoutApply {
    ok(
        compose_layout_apply(
            FOLDER,
            &session_ids(&[ALPHA, BETA]),
            &arranged(),
            &reported(),
            SOCKET,
        ),
        "compose",
    )
}

#[test]
fn an_apply_composed_against_a_tab_that_has_since_moved_re_fetches_and_never_retries() {
    let pending = composed();
    let mut ledger = PendingLayoutApplies::new();
    ledger.begin(pending.clone());
    assert_eq!(ledger.len(), 1);

    // The tab redialled, so the coordinator has no live socket to answer for it.
    let recovery = ledger.settle(
        &pending.target,
        MOVED_SOCKET,
        &LayoutApplyAnswer::TargetGone {
            correlation_id: "corr-1".to_owned(),
        },
    );
    assert_eq!(
        recovery,
        LayoutApplyRecovery::Refetch {
            correlation_id: "corr-1".to_owned(),
            reason: LAYOUT_APPLY_REFETCH_REASON.to_owned(),
        }
    );
    // The ledger is empty, so there is nothing in flight to retry, and the
    // recovery type has no arm that could carry this document forward. A retry
    // loop against a fence is how a browser ends up spinning.
    assert!(ledger.is_empty());
    assert_eq!(ledger.pending(&pending.target), None);
}

#[test]
fn a_refusal_re_fetches_rather_than_re_sending_the_same_document() {
    let pending = composed();
    let recovery = settle_layout_apply(
        &pending,
        &LayoutApplyAnswer::Rejected {
            correlation_id: "corr-1".to_owned(),
            reason: "The target tab is not viewing a live folder.".to_owned(),
        },
    );
    assert!(matches!(recovery, LayoutApplyRecovery::Refetch { .. }));
    // The refused document is still what this client holds; nothing re-sent it,
    // and the recovery carries no document to send.
    assert_eq!(pending.document.bindings.len(), 2);
}

#[test]
fn an_answer_on_a_socket_the_apply_was_not_composed_against_settles_nothing() {
    let pending = composed();
    let mut ledger = PendingLayoutApplies::new();
    ledger.begin(pending.clone());
    // The coordinator already refuses this pairing; a client that read the
    // answer as APPLIED would claim an arrangement the tab never saw.
    let recovery = ledger.settle(
        &pending.target,
        MOVED_SOCKET,
        &LayoutApplyAnswer::Applied {
            correlation_id: "corr-1".to_owned(),
        },
    );
    assert!(matches!(recovery, LayoutApplyRecovery::Refetch { .. }));
    assert!(ledger.is_empty());

    // An answer for a tab this client never applied to settles nothing at all.
    let mut ledger = PendingLayoutApplies::new();
    let stranger = ledger.settle(
        &pending.target,
        SOCKET,
        &LayoutApplyAnswer::Applied {
            correlation_id: "corr-1".to_owned(),
        },
    );
    assert!(matches!(stranger, LayoutApplyRecovery::Unrecognised { .. }));
}

#[test]
fn an_applied_answer_is_the_only_one_that_owes_nothing() {
    let pending = composed();
    assert_eq!(
        settle_layout_apply(
            &pending,
            &LayoutApplyAnswer::Applied {
                correlation_id: "corr-1".to_owned(),
            }
        ),
        LayoutApplyRecovery::Settled {
            correlation_id: "corr-1".to_owned(),
        }
    );
    let mut ledger = PendingLayoutApplies::new();
    ledger.begin(pending.clone());
    assert_eq!(
        ledger.settle(
            &pending.target,
            SOCKET,
            &LayoutApplyAnswer::Applied {
                correlation_id: "corr-1".to_owned(),
            }
        ),
        LayoutApplyRecovery::Settled {
            correlation_id: "corr-1".to_owned(),
        }
    );
    assert!(ledger.is_empty());
}

#[test]
fn a_second_apply_to_the_same_tab_replaces_the_first_rather_than_queueing() {
    let first = composed();
    let mut second = first.clone();
    second.document = single_pane_document(&[ALPHA, BETA, GAMMA], GAMMA);
    let mut ledger = PendingLayoutApplies::new();
    ledger.begin(first.clone());
    ledger.begin(second.clone());
    // One open apply per tab: a second apply to a tab that has not answered the
    // first was composed against state this client has not re-read, and its
    // answer would be matched against the wrong arrangement.
    assert_eq!(ledger.len(), 1);
    assert_eq!(
        ledger
            .pending(&first.target)
            .map(|held| held.document.clone()),
        Some(second.document.clone())
    );
}

#[test]
fn an_arrangement_this_folder_cannot_render_is_never_composed() {
    let mut ids = CountedIds::new("caller");
    let layout = default_layout(&session_ids(&[ALPHA]), &mut ids);
    let mut target = reported();
    target.layout_document = None;
    // No folder bucket, and a duplicated live id: both are refused at compose
    // time, so an apply the coordinator's own parser would reject is never
    // sent.
    assert_eq!(
        compose_layout_apply("", &session_ids(&[ALPHA]), &layout, &target, SOCKET),
        Err(LayoutDocumentError::FolderKeyRequired)
    );
    assert_eq!(
        compose_layout_apply(
            FOLDER,
            &session_ids(&[ALPHA, ALPHA]),
            &layout,
            &target,
            SOCKET
        ),
        Err(LayoutDocumentError::DuplicateLiveSessionId(
            ALPHA.to_owned()
        ))
    );
    // A composed document names only the live sessions, and is one the shared
    // parser admits: the export re-parses before it returns.
    let composed = ok(
        compose_layout_apply(
            FOLDER,
            &session_ids(&[ALPHA, BETA]),
            &layout,
            &target,
            SOCKET,
        ),
        "compose",
    );
    let value = ok(serde_json::to_value(&composed.document), "encode");
    // Not merely `is_ok()`: a document the parser admits but does not return
    // UNCHANGED is one two clients could read differently, so the round trip
    // is asserted to be the identity.
    assert_eq!(
        ok(
            roost_protocol::layout::parse_layout_document_v1(&value),
            "the shared parser admits a composed document"
        ),
        composed.document
    );
    assert_eq!(composed.target.tab_id, TAB);
    assert_eq!(composed.target.socket_id, SOCKET);
    assert_eq!(composed.folder_key, FOLDER);
}
