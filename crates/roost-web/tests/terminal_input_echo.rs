//! The pane's predictive echo on the admission outcome, driven through the
//! real input router and its per-view outcome feed. Pins
//! `components::terminal::pane_echo_feedback` (v2
//! `apps/web/tests/cellTerminalInput.test.ts` "CellTerminal input prediction
//! admission").
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::terminal::input::outcome_feed::ViewAdmission;
use roost_client_core::{InputOutcome, InputPhase, InputRouter};
use roost_web::components::terminal::pane_echo_feedback::{
    EchoFeedback, admission_echo_feedback, outcome_echo_feedback,
};

/// Type one controller keystroke from `view-a`, as the pane does.
fn type_key(router: &mut InputRouter) -> (u64, Option<EchoFeedback>) {
    let admission = router.admit("session-a", Some("view-a".to_owned()), b"a".to_vec(), 0);
    router.outcome_feed.note_admission(Some("view-a"), &admission);
    let answer = router.outcome_feed.take_admission("view-a").unwrap();
    let ViewAdmission::Admitted { input_seq } = answer else {
        panic!("admitted");
    };
    (input_seq, admission_echo_feedback(&answer, true))
}

fn settle(router: &mut InputRouter, outcome: InputOutcome) -> Vec<EchoFeedback> {
    assert!(router.settle(outcome.input_seq(), outcome));
    router
        .outcome_feed
        .take_outcomes("view-a")
        .iter()
        .map(outcome_echo_feedback)
        .collect()
}

#[test]
fn ambiguous_completion_clears_predicted_cells_and_cursor() {
    let mut router = InputRouter::new();
    let (input_seq, placed) = type_key(&mut router);
    assert_eq!(placed, Some(EchoFeedback::Predict { input_seq }));
    let fed = settle(
        &mut router,
        InputOutcome::Ambiguous {
            input_seq,
            written_bytes: 0,
            reason: "connection closed before acknowledgement".to_owned(),
        },
    );
    assert_eq!(
        fed,
        vec![EchoFeedback::Clear {
            status: "ambiguous",
            reason: "connection closed before acknowledgement".to_owned(),
        }]
    );
}

#[test]
fn accepted_completion_preserves_the_prediction_and_acks_its_write() {
    let mut router = InputRouter::new();
    let (input_seq, placed) = type_key(&mut router);
    assert_eq!(placed, Some(EchoFeedback::Predict { input_seq }));
    let fed = settle(
        &mut router,
        InputOutcome::Accepted {
            input_seq,
            written_bytes: 1,
        },
    );
    assert_eq!(fed, vec![EchoFeedback::Written { input_seq }]);
}

#[test]
fn a_refused_keystroke_is_never_predicted_and_clears_what_was() {
    let mut router = InputRouter::new();
    let _ = router.set_phase("session-a", InputPhase::Closed);
    let admission = router.admit("session-a", Some("view-a".to_owned()), b"a".to_vec(), 0);
    router.outcome_feed.note_admission(Some("view-a"), &admission);
    let answer = router.outcome_feed.take_admission("view-a").unwrap();
    assert_eq!(
        admission_echo_feedback(&answer, true),
        Some(EchoFeedback::Clear {
            status: "refused",
            reason: "terminal session is closed".to_owned(),
        })
    );
}

#[test]
fn composed_text_is_acknowledged_but_never_guessed() {
    let answer = ViewAdmission::Admitted { input_seq: 7 };
    assert_eq!(admission_echo_feedback(&answer, false), None);
}
