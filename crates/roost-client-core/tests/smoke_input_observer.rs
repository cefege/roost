//! The smoke input observer as `InputRouter` feeds it: admissions, refusals,
//! every settlement path, and the capture bounds `terminalInputCapture()`
//! reports. Pins `terminal::input::smoke_observer` (v2
//! `apps/web/src/smoke/smokeTerminalInputController.ts` +
//! `apps/web/src/client/carriers/sync-outbound-smoke.ts`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::terminal::input::smoke_observer::{
    ObservedAdmission, SmokeInputObserver, TERMINAL_INPUT_CAPTURE_MAX_BATCHES,
    TERMINAL_INPUT_CAPTURE_MAX_BYTES, TERMINAL_INPUT_OUTCOME_COUNT_CAP,
};
use roost_client_core::terminal::input::{InputRefusal, PendingInput};
use roost_client_core::{InputOutcome, InputPhase, InputRouter, TerminalToken};

fn armed_router() -> InputRouter {
    let mut router = InputRouter::new();
    router.smoke_observer = Some(SmokeInputObserver::new());
    router
}

fn observer(router: &mut InputRouter) -> &mut SmokeInputObserver {
    router.smoke_observer.as_mut().expect("armed")
}

/// Admit the way `handle_terminal_input` does: the router answers, the
/// observer records the answer.
fn admit(
    router: &mut InputRouter,
    session_id: &str,
    bytes: &[u8],
) -> Result<PendingInput, InputRefusal> {
    let admission = router.admit(session_id, None, bytes.to_vec(), 0);
    observer(router).observe_admission(session_id, &admission);
    admission
}

fn token() -> TerminalToken {
    TerminalToken::sync(1, "sock-1", "epoch-1", 1)
}

#[test]
fn an_admitted_batch_is_captured_and_its_accepted_outcome_is_counted_and_collectable() {
    let mut router = armed_router();
    let pending = admit(&mut router, "s1", b"ls\r").unwrap();
    assert_eq!(
        observer(&mut router).take_admission(),
        Some(ObservedAdmission::Admitted {
            input_seq: pending.input_seq
        })
    );
    let accepted = InputOutcome::Accepted {
        input_seq: pending.input_seq,
        written_bytes: 3,
    };
    assert!(router.settle(pending.input_seq, accepted.clone()));

    let capture = observer(&mut router).capture();
    assert_eq!(capture.batches.len(), 1);
    assert_eq!(capture.batches[0].session_id, "s1");
    assert_eq!(capture.batches[0].data, b"ls\r".to_vec());
    assert_eq!(capture.outcomes.accepted, 1);
    assert_eq!(
        observer(&mut router).take_outcome(pending.input_seq),
        Some(accepted)
    );
    assert_eq!(observer(&mut router).take_outcome(pending.input_seq), None);
}

#[test]
fn a_refused_admission_carries_the_routers_reason_and_captures_nothing() {
    let mut router = armed_router();
    let _ = router.set_phase("s1", InputPhase::Closed);
    assert!(admit(&mut router, "s1", b"x").is_err());
    assert_eq!(
        observer(&mut router).take_admission(),
        Some(ObservedAdmission::Refused {
            reason: "terminal session is closed".to_owned()
        })
    );
    assert!(observer(&mut router).capture().batches.is_empty());
}

#[test]
fn route_retirement_and_unstarted_refusal_report_their_outcomes() {
    let mut router = armed_router();
    let started = admit(&mut router, "s1", b"a").unwrap();
    assert!(router.mark_started(started.input_seq, &token()));
    let unstarted = admit(&mut router, "s2", b"b").unwrap();

    let _ = router.retire_token(&token(), "route lost");
    let _ = router.set_phase("s2", InputPhase::Blocked);

    let counts = observer(&mut router).capture().outcomes;
    assert_eq!(
        (counts.accepted, counts.rejected, counts.ambiguous),
        (0, 1, 1)
    );
    assert!(
        observer(&mut router)
            .take_outcome(started.input_seq)
            .unwrap()
            .is_ambiguous()
    );
    assert!(matches!(
        observer(&mut router).take_outcome(unstarted.input_seq),
        Some(InputOutcome::Rejected { .. })
    ));
}

#[test]
fn the_capture_evicts_its_oldest_batch_and_drops_one_too_large_to_keep() {
    let mut capture = SmokeInputObserver::new();
    let mut router = InputRouter::new();
    for _ in 0..=TERMINAL_INPUT_CAPTURE_MAX_BATCHES {
        let admitted = router.admit("s1", None, vec![b'k'], 0);
        capture.observe_admission("s1", &admitted);
        let seq = admitted.unwrap().input_seq;
        let _ = router.settle(
            seq,
            InputOutcome::Accepted {
                input_seq: seq,
                written_bytes: 1,
            },
        );
    }
    let bounded = capture.capture();
    assert_eq!(bounded.batches.len(), TERMINAL_INPUT_CAPTURE_MAX_BATCHES);
    assert_eq!(bounded.dropped_batches, 1);

    let oversized = router.admit("s3", None, vec![0; 64], 0).unwrap();
    let mut huge = oversized.clone();
    huge.bytes = vec![0; TERMINAL_INPUT_CAPTURE_MAX_BYTES + 1];
    capture.observe_admission("s3", &Ok(huge));
    let after = capture.capture();
    assert_eq!(after.dropped_batches, 2);
    assert!(after.batches.iter().all(|batch| batch.session_id == "s1"));
}

#[test]
fn outcome_counters_saturate_and_a_reset_keeps_outcomes_a_probe_still_awaits() {
    let mut capture = SmokeInputObserver::new();
    for seq in 1..=u64::from(TERMINAL_INPUT_OUTCOME_COUNT_CAP) + 5 {
        capture.observe_outcome(&InputOutcome::Rejected {
            input_seq: seq,
            reason: "no".to_owned(),
        });
    }
    assert_eq!(
        capture.capture().outcomes.rejected,
        TERMINAL_INPUT_OUTCOME_COUNT_CAP
    );
    capture.reset_capture();
    assert_eq!(capture.capture().outcomes.rejected, 0);
    let newest = u64::from(TERMINAL_INPUT_OUTCOME_COUNT_CAP) + 5;
    assert!(capture.take_outcome(newest).is_some());
}
