//! The drain-and-answer half of the shell's UI bridge: a coordinator command
//! executed against this tab's store, and acknowledged on the socket it named.
//!
//! The gate these pin is `callerApplyLayout` in the smoke suite, whose caller
//! holds the request open until the answer lands. Silence is the one answer it
//! cannot use, so a refusal the coordinator asked for has to travel back as a
//! refusal — and an answer on the wrong socket is not an answer at all.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod ui_bridge_support;

use roost_client_core::client::ui_command::UI_STATE_REPORT_DEBOUNCE_MS;
use roost_client_core::client::ui_state::LayoutApplyOutcome;

use ui_bridge_support::*;

/// A session id no folder in this store holds.
const FOREIGN: &str = "00000000-0000-4000-8000-0000000000ff";

fn session_path(session_id: &str) -> String {
    format!("/s/{session_id}")
}

/// A bridge showing `ALPHA`, past its first report, with the host emptied.
fn ready_bridge() -> Bridge {
    let bridge = mounted_bridge();
    bridge.show(&session_path(ALPHA), false);
    bridge.state.borrow_mut().start(0);
    bridge.run_at(0);
    bridge.run_at(UI_STATE_REPORT_DEBOUNCE_MS);
    bridge.host.borrow_mut().clear();
    bridge
}

/// THE ANSWER. An apply this tab is the exact target for is committed, reported
/// as applied on its own correlation, and nothing else.
#[test]
fn an_apply_for_this_tab_and_socket_is_answered_once_with_its_own_correlation() {
    let bridge = ready_bridge();
    let document = single_pane_document(&[ALPHA, BETA], ALPHA);
    bridge.apply(
        &session_path(ALPHA),
        &apply_command(OWN_TAB, SOCKET, "corr-apply", Some(&document)),
    );

    let host = bridge.host.borrow();
    assert_eq!(
        host.answers.len(),
        1,
        "the caller holds a request open until this tab answers, once"
    );
    assert_eq!(host.answers[0].correlation_id, "corr-apply");
    assert_eq!(host.answers[0].outcome, LayoutApplyOutcome::Applied);
    assert_eq!(host.answers[0].reason, None);
    assert_eq!(
        host.navigations,
        vec![session_path(ALPHA)],
        "the arrangement the apply committed selects the session it paints"
    );
    assert_eq!(bridge.stored_document(), Some(document));
}

/// THE FENCE, ON THE ANSWER. A command composed against a socket generation this
/// tab has moved off is not answered at all: the coordinator reserved that exact
/// generation, and an answer on this one would settle an arrangement against a
/// tab that never saw it.
#[test]
fn an_apply_naming_a_socket_this_tab_has_moved_off_is_answered_by_nobody() {
    let bridge = ready_bridge();
    let document = single_pane_document(&[ALPHA, BETA], ALPHA);
    bridge.apply(
        &session_path(ALPHA),
        &apply_command(OWN_TAB, PREVIOUS_SOCKET, "corr-stale", Some(&document)),
    );

    let host = bridge.host.borrow();
    assert!(
        host.answers.is_empty(),
        "a stale socket generation is not this tab's to answer on"
    );
    assert!(host.navigations.is_empty());
    assert_eq!(
        bridge.stored_document(),
        None,
        "and nothing was committed for a command this tab is not the target of"
    );
}

/// THE OTHER HALF OF THE FENCE: the reservation names the tab AND the socket.
#[test]
fn an_apply_naming_another_tab_is_answered_by_nobody() {
    let bridge = ready_bridge();
    let document = single_pane_document(&[ALPHA, BETA], ALPHA);
    bridge.apply(
        &session_path(ALPHA),
        &apply_command("tab-somewhere-else", SOCKET, "corr-other", Some(&document)),
    );

    assert!(bridge.host.borrow().answers.is_empty());
    assert_eq!(bridge.stored_document(), None);
}

/// THE REFUSAL. A document that binds a session the folder does not hold is
/// refused BEFORE any mutation, and the refusal is what travels back.
#[test]
fn a_document_the_folder_cannot_admit_is_refused_and_answered() {
    let bridge = ready_bridge();
    let document = foreign_binding_document(FOREIGN);
    bridge.apply(
        &session_path(ALPHA),
        &apply_command(OWN_TAB, SOCKET, "corr-bad", Some(&document)),
    );

    let host = bridge.host.borrow();
    assert_eq!(host.answers.len(), 1, "a refusal is still an answer");
    assert_eq!(host.answers[0].correlation_id, "corr-bad");
    assert_eq!(host.answers[0].outcome, LayoutApplyOutcome::Rejected);
    assert_eq!(
        host.answers[0].reason.as_deref(),
        Some("The layout document is invalid for the current folder."),
        "the caller is told why, in a fixed sentence that names no session"
    );
    assert_eq!(
        bridge.stored_document(),
        None,
        "a refused apply leaves the stored arrangement exactly as it was"
    );
}

/// THE OTHER REFUSAL. A tab showing no live folder cannot arrange one, and says
/// so rather than staying silent until the coordinator's 15s timeout answers
/// `TARGET_GONE` for a tab that is perfectly reachable.
#[test]
fn an_apply_for_a_tab_showing_no_live_folder_is_refused_and_answered() {
    let bridge = ready_bridge();
    let document = single_pane_document(&[ALPHA, BETA], ALPHA);
    bridge.apply(
        "/settings/machines",
        &apply_command(OWN_TAB, SOCKET, "corr-nofolder", Some(&document)),
    );

    let host = bridge.host.borrow();
    assert_eq!(host.answers.len(), 1);
    assert_eq!(host.answers[0].outcome, LayoutApplyOutcome::Rejected);
    assert_eq!(
        host.answers[0].reason.as_deref(),
        Some("The target tab is not viewing a live folder.")
    );
    assert_eq!(bridge.stored_document(), None);
}

/// A frame with no document is the wire's "no document", which is a refusal
/// rather than an empty arrangement.
#[test]
fn an_apply_with_no_document_is_refused_and_answered() {
    let bridge = ready_bridge();
    bridge.apply(
        &session_path(ALPHA),
        &apply_command(OWN_TAB, SOCKET, "corr-empty", None),
    );

    let host = bridge.host.borrow();
    assert_eq!(host.answers.len(), 1);
    assert_eq!(host.answers[0].outcome, LayoutApplyOutcome::Rejected);
    assert_eq!(bridge.stored_document(), None);
}

/// THE DRAIN. A sweep with nothing queued is still a sweep: the report cadence
/// runs on the same pass, and the two do not starve each other.
#[test]
fn a_sweep_drains_the_queue_and_still_answers_the_report_cadence() {
    let bridge = ready_bridge();
    assert_eq!(bridge.queued(), 0);
    bridge.show("/settings/machines", false);

    let now = UI_STATE_REPORT_DEBOUNCE_MS * 2;
    bridge.run_at(now);
    assert!(
        bridge.host.borrow().reports.is_empty(),
        "the change owes a report, not an immediate one"
    );
    bridge.run_at(now + UI_STATE_REPORT_DEBOUNCE_MS);
    assert_eq!(
        bridge.host.borrow().reports.len(),
        1,
        "an empty queue must not cost the tab its report"
    );
}

/// THE TWO HALVES MEET. An arrangement this tab applied is the arrangement it
/// then reports, so the coordinator and the reader converge on one document
/// without either of them re-deriving it.
#[test]
fn an_applied_arrangement_is_the_one_this_tab_reports_next() {
    let bridge = ready_bridge();
    let document = single_pane_document(&[BETA, ALPHA], BETA);
    bridge.apply(
        &session_path(ALPHA),
        &apply_command(OWN_TAB, SOCKET, "corr-then", Some(&document)),
    );
    bridge.host.borrow_mut().clear();

    let now = UI_STATE_REPORT_DEBOUNCE_MS * 2;
    bridge.run_at(now);
    bridge.run_at(now + UI_STATE_REPORT_DEBOUNCE_MS);

    let reports = bridge.host.borrow().reports.clone();
    let report = reports
        .last()
        .expect("a committed arrangement is a change the report owes");
    assert_eq!(report.layout_document, Some(document));
}

// THE OTHER HALF OF THE SAME COMMIT. The deck repaints an applied arrangement
// and the tab reports it, but `LayoutRecords::commit` persists NOTHING — the
// deck's own commit path owns that write, and an arrangement the COORDINATOR
// sent went through no deck commit at all. So `roost.paneLayout.v1` kept the
// reader's earlier arrangement and a reload restored that instead.
#[test]
fn an_applied_arrangement_is_written_where_a_reload_would_read_it() {
    let bridge = ready_bridge();
    assert!(
        bridge.persisted_payload().is_none(),
        "nothing is written before an apply commits one"
    );
    let document = single_pane_document(&[BETA, ALPHA], BETA);
    bridge.apply(
        &session_path(ALPHA),
        &apply_command(OWN_TAB, SOCKET, "corr-persist", Some(&document)),
    );

    let payload = bridge
        .persisted_payload()
        .expect("a committed arrangement survives the reload that follows it");
    let folder = folder_key(&bridge);
    assert!(
        payload.contains(&folder),
        "the persisted record names the folder it arranges: {payload}"
    );
    assert_eq!(
        bridge.stored_document(),
        Some(document),
        "what is persisted is what the tab holds, not a second copy of it"
    );
}
