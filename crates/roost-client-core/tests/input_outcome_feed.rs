//! The per-view input outcome feed the terminal pane's predictive echo reads:
//! the admission answer, then the settled outcome, routed to the view the batch
//! came from. Pins `terminal::input::outcome_feed` (v2
//! `apps/web/src/components/terminal/cell-terminal-input.ts`
//! `reportImmediateRejection` / `watchAcceptedAdmission`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;

use roost_client_core::store::prefs::PrefDefaults;
use roost_client_core::terminal::input::outcome_feed::ViewAdmission;
use roost_client_core::{
    ClientCore, ClientEvent, InputOutcome, InputPhase, InputRouter, KeyValueStore, MemoryClock,
    MemoryKeyValueStore, TerminalToken,
};

fn admit(router: &mut InputRouter, view_id: &str, bytes: &[u8]) -> u64 {
    let admission = router.admit("s1", Some(view_id.to_owned()), bytes.to_vec(), 0);
    router
        .outcome_feed
        .note_admission(Some(view_id), &admission);
    admission.unwrap().input_seq
}

fn token() -> TerminalToken {
    TerminalToken::sync(1, "sock-1", "epoch-1", 1)
}

#[test]
fn a_pane_keystroke_with_no_route_is_admitted_then_reaches_its_view_rejected() {
    let storage: Rc<dyn KeyValueStore> = Rc::new(MemoryKeyValueStore::new());
    let mut core = ClientCore::new(
        Rc::new(MemoryClock::new()),
        storage,
        "tab-feed",
        &PrefDefaults::default(),
    );
    let _ = core.handle(ClientEvent::TerminalInput {
        session_id: "s1".to_owned(),
        view_id: Some("view-a".to_owned()),
        bytes: b"x".to_vec(),
    });
    let feed = &mut core.store_mut().input.outcome_feed;
    let Some(ViewAdmission::Admitted { input_seq }) = feed.take_admission("view-a") else {
        panic!("the pane's keystroke was admitted");
    };
    let outcomes = feed.take_outcomes("view-a");
    assert!(
        matches!(outcomes.as_slice(), [InputOutcome::Rejected { input_seq: seq, .. }] if *seq == input_seq),
        "a batch settled unsent reaches the view as a rejection: {outcomes:?}"
    );
    assert!(
        feed.take_admission("view-a").is_none(),
        "an admission answer is consumed once"
    );
}

#[test]
fn an_acknowledged_batch_reaches_only_the_view_that_typed_it() {
    let mut router = InputRouter::new();
    let typed_a = admit(&mut router, "view-a", b"a");
    let typed_b = admit(&mut router, "view-b", b"b");
    let accepted = InputOutcome::Accepted {
        input_seq: typed_a,
        written_bytes: 1,
    };
    assert!(router.settle(typed_a, accepted.clone()));
    assert_eq!(router.outcome_feed.take_outcomes("view-a"), vec![accepted]);
    assert!(router.outcome_feed.take_outcomes("view-b").is_empty());
    assert!(
        router.outcome_feed.take_outcomes("view-a").is_empty(),
        "a drain empties the view"
    );
    let _ = router.settle(
        typed_b,
        InputOutcome::Accepted {
            input_seq: typed_b,
            written_bytes: 1,
        },
    );
    assert_eq!(router.outcome_feed.take_outcomes("view-b").len(), 1);
}

#[test]
fn a_refused_admission_is_answered_with_the_routers_reason() {
    let mut router = InputRouter::new();
    let _ = router.set_phase("s1", InputPhase::Closed);
    let admission = router.admit("s1", Some("view-a".to_owned()), b"x".to_vec(), 0);
    router
        .outcome_feed
        .note_admission(Some("view-a"), &admission);
    assert_eq!(
        router.outcome_feed.take_admission("view-a"),
        Some(ViewAdmission::Refused {
            reason: "terminal session is closed".to_owned()
        })
    );
}

#[test]
fn route_loss_reaches_the_view_as_ambiguous_and_unstarted_refusal_as_rejected() {
    let mut router = InputRouter::new();
    let started = admit(&mut router, "view-a", b"a");
    assert!(router.mark_started(started, &token(), 0));
    let _ = router.retire_token(&token(), "route lost");
    let unstarted = admit(&mut router, "view-a", b"b");
    let _ = router.set_phase("s1", InputPhase::Blocked);
    let outcomes = router.outcome_feed.take_outcomes("view-a");
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[0].is_ambiguous() && outcomes[0].input_seq() == started);
    assert!(
        matches!(outcomes[1], InputOutcome::Rejected { input_seq, .. } if input_seq == unstarted)
    );
}

#[test]
fn a_started_batch_whose_result_never_comes_settles_ambiguous_at_the_result_timeout() {
    let mut router = InputRouter::new();
    let started = admit(&mut router, "view-a", b"a");
    assert!(router.mark_started(started, &token(), 1_000));
    let held = admit(&mut router, "view-a", b"b");

    assert!(
        router.sweep_unanswered(10_999).is_empty(),
        "the worker has until the full result timeout to answer"
    );
    let settled = router.sweep_unanswered(11_000);
    assert_eq!(settled.len(), 1, "only the batch that left settles");
    assert!(settled[0].is_ambiguous() && settled[0].input_seq() == started);
    let outcomes = router.outcome_feed.take_outcomes("view-a");
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].is_ambiguous() && outcomes[0].input_seq() == started);
    assert!(
        router.sweep_unanswered(30_000).is_empty(),
        "an unsent batch {held} is the hold's to settle, never this timer's"
    );
}

#[test]
fn a_forgotten_view_receives_nothing_for_batches_still_in_flight() {
    let mut router = InputRouter::new();
    let in_flight = admit(&mut router, "view-a", b"a");
    router.outcome_feed.forget_view("view-a");
    assert!(router.outcome_feed.take_admission("view-a").is_none());
    let _ = router.settle(
        in_flight,
        InputOutcome::Accepted {
            input_seq: in_flight,
            written_bytes: 1,
        },
    );
    assert!(router.outcome_feed.take_outcomes("view-a").is_empty());
}

#[test]
fn a_batch_without_a_view_is_never_fed() {
    let mut router = InputRouter::new();
    let admission = router.admit("s1", None, b"x".to_vec(), 0);
    router.outcome_feed.note_admission(None, &admission);
    let seq = admission.unwrap().input_seq;
    let _ = router.settle(
        seq,
        InputOutcome::Accepted {
            input_seq: seq,
            written_bytes: 1,
        },
    );
    assert!(router.outcome_feed.take_outcomes("").is_empty());
}
