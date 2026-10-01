//! The foreground liveness watchdog, end to end: a pane that kept painting is
//! left alone, a pane that stopped painting is challenged, the baseline that
//! answers proves the lane, and a challenge nothing answers escalates through
//! the recovery that already exists.
//!
//! Time is driven EXPLICITLY through `ClientEvent::Sweep`, because this crate
//! has no timer and a sleep would only prove the machine is fast. Retirement and
//! the rearm episode live in `terminal_liveness_retirement.rs`; the fixtures are
//! in `terminal_liveness_support`.
//!
//! Ports the rules of `apps/web/tests/terminalStream.test.ts` and
//! `apps/web/tests/terminalStreamProbeEpisode.test.ts` onto the Rust state
//! machine and the sweep that fires it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_liveness_support;

use roost_client_core::effect::{Effect, SyncCommand};
use roost_client_core::event::ClientEvent;
use roost_client_core::terminal::liveness::{ForegroundLiveness, RepairOutcome};
use roost_client_core::terminal::routes::PromotionCandidate;
use roost_client_core::{Admission, ClientCore, TerminalToken, TerminalTransport};

use terminal_liveness_support::{
    EPOCH, IDLE_PROBE_MS, PROOF_DEADLINE_MS, ROWS, SESSION, STREAM, VIEW, WORKER_FP, admit,
    challenges, deadlines, delta, full, link_closes, outcome, painted, pane_without_baseline,
    repair_attempts, replica_of, replica_with_baseline, sweep, sync_token,
};

/// The elected replica's liveness state, cloned out so a later sweep can run
/// while the read is being asserted on.
fn liveness_of(core: &ClientCore) -> ForegroundLiveness {
    core.store()
        .terminal(SESSION)
        .expect("the pane created a replica")
        .liveness()
        .clone()
}

/// A pane that keeps painting is never challenged: the probe is a statement about
/// silence, so every frame pushes it out.
#[test]
fn a_foreground_view_that_keeps_painting_is_never_challenged() {
    let (mut core, _) = painted(0);
    let mut published = 0;
    // A frame lands inside every one of the eight probe intervals, so the
    // deadline is always re-anchored before it can come due. The sequence each
    // delta continues is the interval index, not a counter of its own.
    for (index, second) in (1..=8u64).enumerate() {
        let now = second * 1_000;
        admit(&mut core, &delta(1 + index as u64, ROWS, 0), now);
        published += challenges(&sweep(&mut core, now + 500)).len();
    }
    published += challenges(&sweep(&mut core, 9_000)).len();
    assert_eq!(
        published, 0,
        "a pane that painted every second owes nobody a challenge"
    );
    assert_eq!(
        repair_attempts(&core),
        0,
        "a healthy pane records no attempt"
    );
}

/// The control for the case above: a pane whose view is PARKED is not the
/// foreground, so however long it is quiet nothing is demanded of it.
#[test]
fn a_parked_view_is_never_challenged_however_long_it_is_quiet() {
    let (mut core, _) = painted(0);
    core.handle(ClientEvent::ViewHidden {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
    });
    for now in [IDLE_PROBE_MS, 30_000, 90_000] {
        assert!(
            challenges(&sweep(&mut core, now)).is_empty(),
            "{now}ms: a parked pane must publish no challenge"
        );
    }
    assert_eq!(deadlines(&core).0, None, "parking retires the probe");
    assert_eq!(outcome(&core), RepairOutcome::Inactive);
}

/// A dropped final frame leaves the producer quiet forever, so the probe is the
/// only thing left that can ask: it publishes one challenge, and the baseline it
/// brings back proves the lane instead of escalating to a redial.
#[test]
fn a_quiet_foreground_pane_challenges_and_the_painted_full_proves_it() {
    let (mut core, generation) = painted(0);
    assert_eq!(
        deadlines(&core),
        (Some(IDLE_PROBE_MS), None, None),
        "an accepted frame arms the probe one interval out"
    );
    let effects = sweep(&mut core, IDLE_PROBE_MS);
    let published = challenges(&effects);
    assert_eq!(
        published.len(),
        1,
        "the probe publishes exactly one challenge"
    );
    match published[0] {
        SyncCommand::TerminalResync {
            session_id,
            view_id,
            stream_id,
            grid_epoch,
            seq,
            ..
        } => assert_eq!(
            (
                session_id.as_str(),
                view_id.as_str(),
                stream_id.as_str(),
                grid_epoch.as_str(),
                *seq
            ),
            (SESSION, VIEW, STREAM, EPOCH, 1),
            "the challenge must be the EXISTING scoped resync, naming this view \
             and this checkpoint"
        ),
        other => panic!("expected a terminal resync, got {other:?}"),
    }
    assert_eq!(
        deadlines(&core),
        (
            None,
            Some(IDLE_PROBE_MS + PROOF_DEADLINE_MS),
            Some(IDLE_PROBE_MS)
        ),
        "a published challenge trades the probe for a proof deadline"
    );
    assert_eq!(repair_attempts(&core), 1);
    assert_eq!(outcome(&core), RepairOutcome::Requested);
    assert!(
        link_closes(&effects).is_empty(),
        "a challenge is not an escalation"
    );

    // The baseline the challenge asked for arrives, on the same stream and past
    // the challenged checkpoint.
    let mut baseline = full(ROWS);
    baseline.seq = 2;
    assert_eq!(
        admit(&mut core, &baseline, IDLE_PROBE_MS + 100),
        Admission::BaselineReplaced
    );
    let proved = liveness_of(&core);
    assert_eq!(
        (
            proved.proof_due_ms(),
            proved.challenged_at_ms(),
            proved.repair_attempts(),
            proved.outcome()
        ),
        (None, None, 1, RepairOutcome::Proved),
        "a later same-stream frame clears the challenge and records the proof"
    );

    // And the proof deadline that was armed is gone, so it cannot escalate.
    let late = sweep(&mut core, IDLE_PROBE_MS + PROOF_DEADLINE_MS);
    assert!(
        link_closes(&late).is_empty(),
        "a challenge that was proved must not replace the socket"
    );
    assert_eq!(
        core.store().sync.link_generation(),
        Some(generation),
        "the socket generation must not move"
    );
}

/// The shape with no other watchdog: the authority installed a stream, the
/// baseline it seeded never arrived, and nothing was ever REFUSED — so no latch
/// fired, no request went out, and the pane sat on its previous grid with a
/// console that said only "expecting a fresh baseline".
///
/// The probe is armed on the EXPECTATION, not on a frame, because this pane has
/// no frame to arm it.
#[test]
fn a_pane_whose_baseline_never_arrived_is_challenged_without_a_frame_ever_landing() {
    let (mut core, generation) = pane_without_baseline();
    let opened = core
        .store()
        .terminal(SESSION)
        .expect("the pane created a replica");
    assert!(
        !opened.baseline_ready(),
        "the fixture's pane must have an expectation and no baseline"
    );
    assert_eq!(opened.expected_stream_id(), Some(STREAM));
    assert_eq!(
        deadlines(&core),
        (None, None, None),
        "no frame has landed, so nothing is armed yet"
    );

    // The FIRST sweep arms the probe one interval out, from that sweep's own
    // instant rather than from a frame that never came.
    assert!(challenges(&sweep(&mut core, 100)).is_empty());
    assert_eq!(deadlines(&core).0, Some(100 + IDLE_PROBE_MS));

    let effects = sweep(&mut core, 100 + IDLE_PROBE_MS);
    let published = challenges(&effects);
    assert_eq!(
        published.len(),
        1,
        "a pane expecting a stream it has no baseline for owes exactly one challenge"
    );
    match published[0] {
        SyncCommand::TerminalResync {
            session_id,
            view_id,
            stream_id,
            grid_epoch,
            seq,
            ..
        } => assert_eq!(
            (
                session_id.as_str(),
                view_id.as_str(),
                stream_id.as_str(),
                grid_epoch.as_str(),
                *seq
            ),
            (SESSION, VIEW, STREAM, "", 0),
            "with nothing painted there is no checkpoint to name but the stream"
        ),
        other => panic!("expected a terminal resync, got {other:?}"),
    }
    assert_eq!(
        deadlines(&core),
        (
            None,
            Some(100 + IDLE_PROBE_MS + PROOF_DEADLINE_MS),
            Some(100 + IDLE_PROBE_MS)
        ),
        "the challenge trades the probe for a proof deadline"
    );

    // One challenge, not one per sweep: the proof deadline owns the session, so
    // what comes next is the escalation and not another request.
    let mut repeats = 0;
    for now in [5_500, 6_000, 7_000, 8_000] {
        repeats += challenges(&sweep(&mut core, now)).len();
    }
    assert_eq!(repeats, 0, "an outstanding challenge is the sole coalescer");
    assert_eq!(
        link_closes(&sweep(&mut core, 100 + IDLE_PROBE_MS + PROOF_DEADLINE_MS)),
        vec![(generation, "terminal-liveness".to_owned())],
        "a baseline that never came is a proof that never came"
    );
}

/// A frame from a generation this replica is not bound to is not a frame, so it
/// satisfies nothing: neither the quiet deadline nor an outstanding challenge.
#[test]
fn a_frame_from_another_generation_satisfies_neither_deadline() {
    let (mut core, _) = painted(0);
    let foreign = TerminalToken::sync(1, "sock-elsewhere", "epoch-elsewhere", 3);
    let mut late = full(ROWS);
    late.seq = 2;
    assert!(
        matches!(
            replica_of(&mut core).admit_frame(&late, false, &foreign, 1_000),
            Admission::Refused { latched: false, .. }
        ),
        "the fixture's foreign frame must not be admissible"
    );
    assert_eq!(
        deadlines(&core),
        (Some(IDLE_PROBE_MS), None, None),
        "a foreign frame re-anchors nothing: the probe still comes due when it was \
         armed to"
    );
    assert_eq!(challenges(&sweep(&mut core, IDLE_PROBE_MS)).len(), 1);

    // The same rule on the other deadline: a frame ACCEPTED on a new generation
    // does not prove a challenge the OLD generation issued.
    let mut replica = replica_with_baseline(ROWS);
    let first = sync_token(1, 1);
    replica.begin_scoped_repair(&first, 100);
    let second = sync_token(2, 1);
    replica.bind_generation(&second);
    let mut newer = full(ROWS);
    newer.seq = 9;
    assert_eq!(
        replica.admit_frame(&newer, false, &second, 200),
        Admission::BaselineReplaced
    );
    assert!(
        replica.liveness().challenged_at_ms().is_some(),
        "a challenge names the generation it went out on, so a frame from another \
         one cannot answer it"
    );
}

/// A staged candidate folds into its OWN replica on its OWN generation, and that
/// replica is not the elected one. Only the elected owner owes — and is owed — a
/// probe, so a candidate's armed state must never be consumed.
#[test]
fn only_the_elected_replica_is_probed_and_a_staged_candidate_is_not() {
    let (mut core, _) = painted(0);
    let staged_token = TerminalToken::direct(9, TerminalTransport::Peer, WORKER_FP, "epoch-1", 1);
    let mut candidate_replica = replica_with_baseline(ROWS);
    candidate_replica.bind_generation(&staged_token);
    candidate_replica.arm_quiet_probe(&staged_token, 0);
    assert_eq!(
        candidate_replica.liveness().quiet_due_ms(),
        Some(IDLE_PROBE_MS),
        "the fixture's staged replica must be armed, or the sweep ignoring it proves nothing"
    );
    core.store_mut().routes.stage(
        PromotionCandidate {
            session_id: SESSION.to_owned(),
            connection_id: "conn-staged".to_owned(),
            token: staged_token.clone(),
            attempt_id: 1,
            baseline_ready: true,
            prospective_views: Default::default(),
            // Staged four seconds ago, so the staging lane's own baseline
            // deadline does not reap it mid-case: that deadline is a different
            // rule, and this case is about which replica the sweep visits.
            staged_at_ms: 4_000,
        },
        candidate_replica,
    );

    sweep(&mut core, IDLE_PROBE_MS);
    assert!(
        deadlines(&core).2.is_some(),
        "the ELECTED replica is the one that owes a challenge"
    );
    for now in [6_000, 8_000, 8_900] {
        sweep(&mut core, now);
    }
    let staged = core
        .store_mut()
        .routes
        .staged_replica_mut(SESSION)
        .expect("the candidate is still staged");
    assert_eq!(
        (
            staged.liveness().quiet_due_ms(),
            staged.liveness().challenged_at_ms(),
            staged.liveness().repair_attempts()
        ),
        (Some(IDLE_PROBE_MS), None, 0),
        "the sweep must never consume a staged candidate's deadlines"
    );
}

/// A proof that never arrives escalates through the recovery that ALREADY
/// replaces a Sync socket — one `CloseSyncLink` with the liveness reason, and no
/// second path invented beside it.
#[test]
fn an_unanswered_proof_invokes_the_existing_sync_generation_recovery() {
    let (mut core, generation) = painted(0);
    sweep(&mut core, IDLE_PROBE_MS);
    assert_eq!(repair_attempts(&core), 1);

    let effects = sweep(&mut core, IDLE_PROBE_MS + PROOF_DEADLINE_MS);
    assert_eq!(
        link_closes(&effects),
        vec![(generation, "terminal-liveness".to_owned())],
        "a timed-out proof must replace the socket through the one existing path"
    );
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::DialSync { .. }))
            .count(),
        0,
        "the recovery closes the link and lets the existing redial loop dial"
    );
    assert_eq!(outcome(&core), RepairOutcome::Escalated);
    assert_eq!(
        deadlines(&core).1,
        None,
        "the escalation discharges the proof it just paid out"
    );
}
