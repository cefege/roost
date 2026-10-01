//! Every exit from the foreground liveness watchdog, and the two ends of an
//! unpublishable-challenge EPISODE.
//!
//! The rule under test is `docs/FAILURE-INDEX.md` "The liveness watchdog
//! deletes itself on the failure it exists to notice": a pane that stopped
//! painting has no other watchdog, so no path may leave it holding neither a
//! probe nor a proof deadline. Time is driven EXPLICITLY through
//! `ClientEvent::Sweep`; the fixtures are in `terminal_liveness_support`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_liveness_support;

use roost_client_core::event::ClientEvent;
use roost_client_core::terminal::liveness::RepairOutcome;
use roost_client_core::{ClientCore, TerminalToken};

use terminal_liveness_support::{
    IDLE_PROBE_MS, OTHER_STREAM, PROOF_DEADLINE_MS, ROWS, SESSION, VIEW, arm_both, challenges,
    deadlines, open_ready_link, outcome, painted, repair_attempts, replica_of, sweep, token,
};

/// A challenge that could not be published leaves the pane owing a DEADLINE, and
/// that deadline is anchored at the instant the attempt failed — never at the
/// frame timestamp that is already older than the interval, which is the spin
/// `docs/FAILURE-INDEX.md` records under this feature.
#[test]
fn an_unpublishable_challenge_re_arms_from_the_current_instant_and_never_spins() {
    let (mut core, _) = painted(0);
    // A challenge this generation already issued is the coalescer: the probe
    // cannot publish a second one, so this is the unpublishable path. Its proof
    // deadline is placed AFTER the probe's, so the probe is what comes due.
    let current = token(&core);
    let challenge_ms = IDLE_PROBE_MS - 500;
    replica_of(&mut core).begin_scoped_repair(&current, challenge_ms);
    assert_eq!(deadlines(&core).2, Some(challenge_ms));

    sweep(&mut core, IDLE_PROBE_MS);
    let rearmed = Some(IDLE_PROBE_MS + IDLE_PROBE_MS);
    assert_eq!(
        deadlines(&core).0,
        rearmed,
        "the re-arm is anchored at the instant the attempt failed, not at the frame \
         that armed the first probe {IDLE_PROBE_MS}ms earlier"
    );
    assert_eq!(
        deadlines(&core).2,
        Some(challenge_ms),
        "the pending challenge is still the pending challenge"
    );

    // Two more sweeps at the SAME instant must leave it identical: a sweep that
    // re-armed per pass would land in the past and fire again immediately.
    for _ in 0..2 {
        sweep(&mut core, IDLE_PROBE_MS);
    }
    assert_eq!(
        deadlines(&core).0,
        rearmed,
        "a re-arm anchored per sweep would fire again at once, which is the spin"
    );
}

/// Every transition that takes the pane out from under the watchdog retires both
/// deadlines, so nothing fires against a replica that has moved on.
#[test]
fn park_rotation_stream_generation_and_page_hide_each_retire_both_deadlines() {
    // Parked: the view stops asking the authority to show it, so nothing is owed.
    let (mut core, _) = painted(0);
    arm_both(&mut core, 1_000, 1_000);
    core.handle(ClientEvent::ViewHidden {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
    });
    sweep(&mut core, 100_000);
    assert_eq!(
        deadlines(&core),
        (None, None, None),
        "parking must retire both"
    );
    assert_eq!(outcome(&core), RepairOutcome::Inactive);

    // Hidden document: silence while nobody is looking is not a stall.
    let (mut core, _) = painted(0);
    arm_both(&mut core, 1_000, 1_000);
    core.handle(ClientEvent::PageVisibilityChanged { visible: false });
    sweep(&mut core, 100_000);
    assert_eq!(
        deadlines(&core),
        (None, None, None),
        "a hidden page must retire both"
    );
    assert_eq!(outcome(&core), RepairOutcome::Inactive);

    // The carrier generation changed: the challenge named a socket that is gone.
    let (mut core, _) = painted(0);
    arm_both(&mut core, 1_000, 1_000);
    let moved = TerminalToken::sync(77, "sock-moved", "epoch-moved", 3);
    assert!(replica_of(&mut core).bind_generation(&moved));
    sweep(&mut core, 100_000);
    assert_eq!(
        deadlines(&core),
        (None, None, None),
        "a generation change must retire both"
    );
    assert_eq!(outcome(&core), RepairOutcome::GenerationReset);

    // The authority replaced the stream: the challenge named the one before it.
    let (mut core, _) = painted(0);
    arm_both(&mut core, 1_000, 1_000);
    replica_of(&mut core).install_expected_stream(OTHER_STREAM, 8, ROWS);
    sweep(&mut core, 100_000);
    assert_eq!(
        deadlines(&core),
        (None, None, None),
        "a stream change must retire both"
    );
    assert_eq!(outcome(&core), RepairOutcome::StreamReplaced);

    // The Sync socket rotated: the publication target moved on, so the challenge
    // names a socket that no longer exists.
    let (mut core, first) = painted(0);
    arm_both(&mut core, 1_000, 1_000);
    let second = open_ready_link(&mut core);
    assert_ne!(first, second, "the fixture must actually rotate");
    sweep(&mut core, 100_000);
    assert_eq!(
        deadlines(&core),
        (None, None, None),
        "a rotation must retire both"
    );
    assert_eq!(outcome(&core), RepairOutcome::GenerationReset);
}

/// One gap is one challenge. The pending-challenge gate is the sole coalescer,
/// so a second request for the same gap re-anchors the proof it already owes
/// rather than opening another, and the probe firing behind it publishes nothing
/// new.
#[test]
fn a_second_request_for_one_gap_re_anchors_its_proof_and_publishes_no_second_challenge() {
    let (mut core, _) = painted(0);
    let current = token(&core);
    let replica = replica_of(&mut core);
    replica.begin_scoped_repair(&current, 1_000);
    replica.begin_scoped_repair(&current, 2_500);
    assert_eq!(
        deadlines(&core),
        (
            Some(IDLE_PROBE_MS),
            Some(2_500 + PROOF_DEADLINE_MS),
            Some(2_500)
        ),
        "the second request re-anchors the challenge in place"
    );
    assert_eq!(
        repair_attempts(&core),
        2,
        "both requests are recorded as attempts"
    );
    assert!(
        challenges(&sweep(&mut core, IDLE_PROBE_MS)).is_empty(),
        "the probe behind an outstanding challenge must publish nothing"
    );
    assert_eq!(
        deadlines(&core).2,
        Some(2_500),
        "and the challenge it was waiting on survives"
    );
}

/// The rearm is reported as an EPISODE, and BOTH ways an episode can end — a
/// published challenge and retirement — must clear the edge. A flag surviving
/// either one silences the first rearm of the NEXT episode, which is the only one
/// that gets reported.
#[test]
fn the_rearm_episode_reports_once_and_reopens_after_both_of_its_ends() {
    fn claim(core: &mut ClientCore) -> bool {
        replica_of(core).liveness_mut().claim_rearm_episode()
    }

    let (mut core, _) = painted(0);
    assert!(claim(&mut core), "the first retry of an episode reports");
    assert!(
        !claim(&mut core),
        "a retry inside the same episode is silent"
    );
    assert_eq!(
        challenges(&sweep(&mut core, IDLE_PROBE_MS)).len(),
        1,
        "the fixture's challenge must publish"
    );
    assert!(
        claim(&mut core),
        "a challenge that published must let the NEXT episode report again"
    );

    // Retirement ends an episode just as surely, which is the path the v2 episode
    // test drives through a hidden pane.
    let (mut core, _) = painted(0);
    assert!(claim(&mut core));
    core.handle(ClientEvent::PageVisibilityChanged { visible: false });
    sweep(&mut core, 100_000);
    assert!(
        claim(&mut core),
        "a flag surviving retirement silences the only rearm that would report"
    );
}
