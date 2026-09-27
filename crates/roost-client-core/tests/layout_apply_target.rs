//! The exact-target gate an acknowledged apply executes under, and what this
//! tab does with the command once it owns it.
//!
//! Mirrors `apps/web/tests/uiLayoutApply.test.ts`: exact tab/socket targeting,
//! validation before mutation, one commit, a bounded settlement diagnostic, the
//! re-read identity before acknowledging, and the no-bridge answer. The eight
//! legacy UI commands stay outside this path, which is the separation the
//! `NotMine` arm exists to make visible.

mod layout_support;
use roost_client_core::client::ui_state::{
    LAYOUT_APPLY_SETTLED_EVENT, LayoutApplyCommand, LayoutApplyConsumption, LayoutApplyExecution,
    LayoutApplyRejection, execute_targeted_layout_apply, reject_layout_apply_without_bridge,
};
use roost_client_core::store::layout::apply_layout_document;

use layout_support::{CountedIds, RecordingHost, ok, session_ids, single_pane_document};

const ALPHA: &str = "alpha";
const BETA: &str = "beta";
const DEAD: &str = "not-live";
const FOLDER: &str = "worker::/work";
const TAB: &str = "tab-current";
const SOCKET: &str = "socket-current";
const OTHER_TAB: &str = "tab-other";
const OTHER_SOCKET: &str = "socket-old";
const CORRELATION: &str = "correlation-1";

fn recording_host() -> RecordingHost {
    RecordingHost::new(
        TAB,
        SOCKET,
        RecordingHost::folder(FOLDER, ALPHA, &[ALPHA, BETA]),
    )
}

fn command() -> LayoutApplyCommand {
    LayoutApplyCommand {
        target_tab_id: TAB.to_owned(),
        target_socket_id: SOCKET.to_owned(),
        correlation_id: CORRELATION.to_owned(),
        document: Some(single_pane_document(&[ALPHA, BETA], BETA)),
    }
}

fn seed(host: &mut RecordingHost) {
    let mut ids = CountedIds::new("seed");
    ok(
        apply_layout_document(
            &mut host.records,
            FOLDER,
            &single_pane_document(&[ALPHA, BETA], ALPHA),
            &session_ids(&[ALPHA, BETA]),
            &mut ids,
        ),
        "seed a stored arrangement",
    );
}

#[test]
fn an_exact_apply_commits_once_clears_spotlight_navigates_and_answers_applied() {
    let mut host = recording_host();
    let execution = execute_targeted_layout_apply(Some(&command()), &mut host);
    assert_eq!(
        execution,
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Applied {
            selected_session_id: Some(BETA.to_owned()),
        })
    );
    assert_eq!(
        host.events,
        vec![
            "spotlight".to_owned(),
            format!("navigate:{BETA}"),
            "ack:applied".to_owned(),
        ]
    );
    assert_eq!(host.results.len(), 1);
    assert_eq!(host.results[0].correlation_id, CORRELATION);
    assert_eq!(host.results[0].reason, None);
    // One settlement line, carrying the correlation and the outcome and nothing
    // else: a diagnostic that could see the document would be a second copy of
    // the arrangement in a log.
    assert_eq!(host.diagnostics.len(), 1);
    assert_eq!(host.diagnostics[0].0, LAYOUT_APPLY_SETTLED_EVENT);
    assert_eq!(host.diagnostics[0].1.outcome, "applied");
}

#[test]
fn a_settlement_diagnostic_bounds_the_correlation_and_the_answer_does_not() {
    let long = "🦆".repeat(129);
    let mut host = recording_host();
    let mut frame = command();
    frame.correlation_id = long.clone();
    execute_targeted_layout_apply(Some(&frame), &mut host);
    // The whole id travels on the answer, because the coordinator correlates on
    // it; only the diagnostic is bounded, because only the diagnostic is a log.
    assert_eq!(host.results[0].correlation_id, long);
    assert_eq!(host.diagnostics[0].1.correlation_id, "🦆".repeat(128));
}

#[test]
fn a_frame_this_tab_is_not_the_exact_target_for_is_consumed_and_answers_nothing() {
    let variants = [
        (
            "another tab",
            LayoutApplyCommand {
                target_tab_id: OTHER_TAB.to_owned(),
                ..command()
            },
        ),
        (
            "a socket that has moved",
            LayoutApplyCommand {
                target_socket_id: OTHER_SOCKET.to_owned(),
                ..command()
            },
        ),
        (
            "a broadcast",
            LayoutApplyCommand {
                target_tab_id: String::new(),
                ..command()
            },
        ),
        (
            "no correlation to answer on",
            LayoutApplyCommand {
                correlation_id: String::new(),
                ..command()
            },
        ),
        (
            "no socket",
            LayoutApplyCommand {
                target_socket_id: String::new(),
                ..command()
            },
        ),
    ];
    for (name, frame) in variants {
        let mut host = recording_host();
        seed(&mut host);
        let before = ok(host.records.snapshot(), "snapshot");
        assert_eq!(
            execute_targeted_layout_apply(Some(&frame), &mut host),
            LayoutApplyExecution::Ignored,
            "{name} was not ignored"
        );
        // A frame that is not this tab's is not even decoded: a stale tab must
        // not spend an apply, a mint, or a write on somebody else's command.
        assert!(host.results.is_empty(), "{name} answered");
        assert!(host.diagnostics.is_empty(), "{name} settled");
        assert!(host.events.is_empty(), "{name} ran side effects");
        assert_eq!(
            ok(host.records.snapshot(), "snapshot"),
            before,
            "{name} changed state"
        );
    }
}

#[test]
fn a_frame_that_is_not_an_apply_frame_is_left_to_the_legacy_command_path() {
    let mut host = recording_host();
    assert_eq!(
        execute_targeted_layout_apply(None, &mut host),
        LayoutApplyExecution::NotMine
    );
    assert_eq!(
        reject_layout_apply_without_bridge(None, &mut host),
        LayoutApplyExecution::NotMine
    );
    assert!(host.events.is_empty());
}

#[test]
fn a_tab_that_is_not_on_a_live_folder_is_refused_before_anything_is_written() {
    // Not viewing a folder at all.
    let mut host = recording_host();
    host.folder = None;
    assert!(matches!(
        execute_targeted_layout_apply(Some(&command()), &mut host),
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Rejected(
            LayoutApplyRejection::NoActiveFolder
        ))
    ));
    // Silence is the one answer the caller cannot use: the whole refusal IS
    // the acknowledgement, and nothing else ran.
    assert_eq!(host.events, vec!["ack:rejected".to_owned()]);

    // The active session is not among the live ones: a folder mid-navigation is
    // not a folder to rearrange.
    let mut host = recording_host();
    host.folder = Some(RecordingHost::folder(FOLDER, ALPHA, &[BETA]));
    assert!(matches!(
        execute_targeted_layout_apply(Some(&command()), &mut host),
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Rejected(
            LayoutApplyRejection::NoActiveFolder
        ))
    ));
    assert_eq!(host.events, vec!["ack:rejected".to_owned()]);

    // The membership is still only this browser's: an arrangement computed
    // against a session the fleet has not admitted is one nobody else can read.
    let mut optimistic = RecordingHost::folder(FOLDER, ALPHA, &[ALPHA, BETA]);
    optimistic.has_client_only_session = true;
    let mut host = recording_host();
    host.folder = Some(optimistic);
    assert!(matches!(
        execute_targeted_layout_apply(Some(&command()), &mut host),
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Rejected(
            LayoutApplyRejection::NoActiveFolder
        ))
    ));
    assert_eq!(host.events, vec!["ack:rejected".to_owned()]);
    assert!(host.records.is_empty());
}

#[test]
fn a_refused_apply_leaves_local_state_byte_identical_to_before_it() {
    let mut host = recording_host();
    seed(&mut host);
    let before = ok(host.records.snapshot(), "snapshot");

    // A document binding a session this folder does not hold.
    let mut frame = command();
    frame.document = Some(single_pane_document(&[ALPHA, DEAD], DEAD));
    assert!(matches!(
        execute_targeted_layout_apply(Some(&frame), &mut host),
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Rejected(
            LayoutApplyRejection::InvalidDocument
        ))
    ));
    // Not one pane moved, and no follow-up ran: the whole rejection is the
    // acknowledgement.
    assert_eq!(ok(host.records.snapshot(), "snapshot"), before);
    assert_eq!(host.events, vec!["ack:rejected".to_owned()]);
    assert_eq!(
        host.results[0].reason.as_deref(),
        Some(LayoutApplyRejection::InvalidDocument.message())
    );
    // The reason that reached the caller is the fixed sentence, never the
    // validation error: this text is painted in somebody else's browser.
    assert!(
        !host.results[0]
            .reason
            .clone()
            .unwrap_or_default()
            .contains(DEAD)
    );
}

#[test]
fn a_commit_that_lands_after_this_tab_redialled_stands_and_answers_nothing() {
    let mut host = recording_host().replacing_identity("socket-next");
    let execution = execute_targeted_layout_apply(Some(&command()), &mut host);
    assert_eq!(
        execution,
        LayoutApplyExecution::Settled(LayoutApplyConsumption::AppliedUnacknowledged {
            selected_session_id: Some(BETA.to_owned()),
        })
    );
    // The commit stands: the answer to it cannot be given on the successor's
    // behalf, and the coordinator's ledger expires the reservation rather than
    // settling it against a tab that is no longer this one.
    assert_eq!(host.records.len(), 1);
    assert!(host.results.is_empty());
    assert_eq!(host.diagnostics[0].1.outcome, "applied");
    assert_eq!(host.socket_id.as_deref(), Some("socket-next"));
}

#[test]
fn a_shell_with_no_router_still_answers_an_exact_apply_and_nothing_else() {
    let mut host = recording_host();
    assert!(matches!(
        reject_layout_apply_without_bridge(Some(&command()), &mut host),
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Rejected(
            LayoutApplyRejection::BridgeUnavailable
        ))
    ));
    assert_eq!(
        host.results[0].reason.as_deref(),
        Some(LayoutApplyRejection::BridgeUnavailable.message())
    );
    assert!(host.records.is_empty());

    // A frame for a tab that has moved must not answer for its predecessor.
    let frame = LayoutApplyCommand {
        target_tab_id: OTHER_TAB.to_owned(),
        ..command()
    };
    let mut host = recording_host();
    assert_eq!(
        reject_layout_apply_without_bridge(Some(&frame), &mut host),
        LayoutApplyExecution::Ignored
    );
    assert!(host.results.is_empty());
}

#[test]
fn a_document_aimed_at_this_tab_with_nothing_in_it_is_still_answered() {
    // The caller is holding a request open until this tab says what happened,
    // and an absent document is an answer, not a silence.
    let frame = LayoutApplyCommand {
        document: None,
        ..command()
    };
    let mut host = recording_host();
    assert!(matches!(
        execute_targeted_layout_apply(Some(&frame), &mut host),
        LayoutApplyExecution::Settled(LayoutApplyConsumption::Rejected(
            LayoutApplyRejection::InvalidDocument
        ))
    ));
    assert!(host.records.is_empty());
    assert_eq!(host.events, vec!["ack:rejected".to_owned()]);
}
