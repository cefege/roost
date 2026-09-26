//! Full-before-delta: one complete authoritative full, exactly once, before any
//! delta — and the ordering trap that makes that true.
//!
//! The trap is named in `docs/phase4-client-contract.md` §6.2:
//! `roost_protocol::cell::apply_delta` REPLACES its base when handed a frame with
//! `full == true`. That is right for the server-side emitter, which has no
//! canonical to protect, and wrong for a replica: fold on `full` first, or a
//! stale full silently undoes the generation that answered it. The case
//! `a_stale_full_cannot_undo_the_generation_that_answered_it` drives that through
//! the public entry point, so a reordering of the fold's two arms fails here.

mod support;

use roost_client_core::Admission;
use support::{NEXT_EPOCH, OTHER_STREAM, SESSION, STREAM, bound_replica, delta, full, sync_token};

#[test]
fn a_delta_before_any_full_is_refused() {
    // The replica has an expectation and a view but NO baseline. A delta here is
    // not a "first delta": it is a refusal, and a refusal latches one repair.
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    let outcome = replica.admit_frame(&delta(0, 4, 1), false, &token, 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "a delta with no baseline must latch a repair, got {outcome:?}"
    );
    assert!(!replica.baseline_ready());
    assert!(replica.canonical().is_none(), "nothing may be painted");
}

#[test]
fn one_complete_full_is_admitted_and_makes_the_replica_paintable() {
    let mut replica = bound_replica(4);
    let outcome = replica.admit_frame(&full(4), false, &sync_token(1, 1), 10);
    assert_eq!(outcome, Admission::BaselineReplaced);
    assert!(replica.baseline_ready());
    assert_eq!(replica.expected_stream_id(), Some(STREAM));
    assert_eq!(replica.canonical().map(|frame| frame.seq), Some(1));
}

#[test]
fn a_second_full_on_the_same_stream_replaces_rather_than_appends() {
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    replica.admit_frame(&full(4), false, &token, 10);
    let mut second = full(4);
    second.seq = 5;
    assert_eq!(
        replica.admit_frame(&second, false, &token, 11),
        Admission::BaselineReplaced
    );
    assert_eq!(replica.canonical().map(|frame| frame.seq), Some(5));
}

/// A full for a DIFFERENT stream is not this replica's frame, and latching a
/// repair for it would ask the authority for something it already sent.
#[test]
fn a_full_for_another_stream_is_dropped_without_latching() {
    let mut replica = bound_replica(4);
    let mut other = full(4);
    other.stream_id = OTHER_STREAM.to_owned();
    let outcome = replica.admit_frame(&other, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: false, .. }),
        "got {outcome:?}"
    );
    assert!(!replica.baseline_ready());
}

#[test]
fn a_full_whose_row_count_disagrees_with_the_pane_is_refused() {
    // The authority computes a minimum across views; a client that accepted a
    // grid its pane cannot show would keep folding deltas against the wrong
    // `rows` count from then on.
    let mut replica = bound_replica(4);
    let outcome = replica.admit_frame(&full(6), false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "got {outcome:?}"
    );
    assert!(!replica.baseline_ready());
}

#[test]
fn a_full_that_claims_a_base_sequence_is_refused() {
    // `base_seq` on a full is a delta wearing a full's flag.
    let mut lying = full(4);
    lying.base_seq = 1;
    let mut replica = bound_replica(4);
    let outcome = replica.admit_frame(&lying, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "got {outcome:?}"
    );
}

#[test]
fn a_full_with_a_mis_numbered_viewport_row_is_refused() {
    // The renderer addresses rows by index, so a mis-numbered full paints the
    // wrong line in the wrong place — permanently, because every later delta
    // folds onto that numbering.
    let mut misnumbered = full(4);
    misnumbered.viewport_rows[2].index = 3;
    let mut replica = bound_replica(4);
    let outcome = replica.admit_frame(&misnumbered, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "got {outcome:?}"
    );
}

#[test]
fn a_full_that_is_also_an_append_is_refused() {
    let mut lying = full(4);
    lying.scrollback_append = vec![support::row(0, "new")];
    let mut replica = bound_replica(4);
    let outcome = replica.admit_frame(&lying, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "got {outcome:?}"
    );
}

#[test]
fn a_full_whose_history_window_does_not_reach_its_total_is_refused() {
    // A gap in the window means an absolute row index re-aliases at the
    // scrollback cap and the reader cannot tell which line it is looking at.
    let mut truncated = full(4);
    truncated.scrollback_total = 3;
    truncated.sb_base = 0;
    truncated.scrollback_rows = vec![support::row(0, "a")];
    let mut replica = bound_replica(4);
    let outcome = replica.admit_frame(&truncated, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "got {outcome:?}"
    );
}

#[test]
fn a_stale_full_cannot_undo_the_generation_that_answered_it() {
    // THE ORDERING TRAP. A snapshot request issued on generation A can still be
    // answered on generation B. If the fold reached `apply_delta` with the stale
    // full — which is legal for the shared helper, because it replaces its base
    // wholesale — it would install A's grid over B's and the session would show
    // the wrong PTY forever.
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    // Generation A's baseline: seq 1, epoch E1.
    let mut generation_a = full(4);
    generation_a.seq = 1;
    assert_eq!(
        replica.admit_frame(&generation_a, false, &token, 10),
        Admission::BaselineReplaced
    );

    // Generation B takes over and re-baselines on a new stream and epoch.
    let mut generation_b = full(4);
    generation_b.stream_id = "00000000-0000-4000-8000-0000000000b1".to_owned();
    generation_b.grid_epoch = NEXT_EPOCH.to_owned();
    generation_b.seq = 1;
    replica.install_expected_stream("00000000-0000-4000-8000-0000000000b1", 8, 4);
    assert_eq!(
        replica.admit_frame(&generation_b, false, &token, 11),
        Admission::BaselineReplaced
    );

    // The snapshot request is answered LATE: a complete, well-formed baseline for
    // B's own stream and epoch, but computed against an EARLIER point than the
    // one B is on. Every `valid_full` clause passes, so the only thing that can
    // refuse it is the ordering — which is the whole point of the case.
    let mut late = full(4);
    late.stream_id = "00000000-0000-4000-8000-0000000000b1".to_owned();
    late.grid_epoch = NEXT_EPOCH.to_owned();
    late.seq = 0;
    let outcome = replica.admit_frame(&late, false, &token, 12);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "a full that moves the replica backwards must be refused, got {outcome:?}"
    );
    let canonical = replica.canonical().expect("B's grid is still installed");
    assert_eq!(canonical.grid_epoch, NEXT_EPOCH, "B's grid survived");
    assert_eq!(canonical.stream_id, "00000000-0000-4000-8000-0000000000b1");
}

#[test]
fn one_gap_latches_exactly_one_repair() {
    // The frames that follow a refused delta are the REST OF THE SAME GAP, not
    // new gaps. Queueing a request for each is how one lost baseline becomes a
    // request storm.
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    let latched = (0..5)
        .filter(|_| {
            let outcome = replica.admit_frame(&delta(99, 4, 1), false, &token, 10);
            matches!(outcome, Admission::Refused { latched: true, .. })
        })
        .count();
    assert_eq!(latched, 1, "five refusals of one gap latch one repair");
    assert!(replica.repair_latched());
}

#[test]
fn only_a_full_clears_the_latch() {
    // An accepted delta proves the LANE, not the hole: the sequence continued
    // across the gap, so the gap is still there and the next full is still owed.
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    replica.admit_frame(&full(4), false, &token, 10);
    let gap = replica.admit_frame(&delta(99, 4, 1), false, &token, 11);
    assert!(matches!(gap, Admission::Refused { latched: true, .. }));
    assert!(replica.repair_latched());

    // A delta that DOES continue cannot be admitted, because the gap is
    // unhealed, so the latch stands. The full that repairs it clears the latch.
    let mut repair = full(4);
    repair.seq = 200;
    assert_eq!(
        replica.admit_frame(&repair, false, &token, 12),
        Admission::BaselineReplaced
    );
    assert!(!replica.repair_latched(), "a full clears the gap");
}

#[test]
fn a_new_stream_drops_the_baseline_and_the_latch() {
    // Stream re-mints are routine: any other viewer joining, leaving, parking or
    // waking re-mints for everyone. A client that kept its baseline across one
    // would be splicing two grids.
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    replica.admit_frame(&full(4), false, &token, 10);
    replica.admit_frame(&delta(99, 4, 1), false, &token, 11);
    assert!(replica.repair_latched());

    let changed = replica.install_expected_stream(OTHER_STREAM, 8, 4);
    assert!(
        changed,
        "a new stream id is a change the host must hear about"
    );
    assert!(
        !replica.baseline_ready(),
        "the old baseline is not this stream's"
    );
    assert!(
        !replica.repair_latched(),
        "the old gap is not this stream's"
    );
    assert_eq!(replica.expected_stream_id(), Some(OTHER_STREAM));
}

#[test]
fn a_frame_for_another_session_never_lands_here() {
    let mut replica = bound_replica(4);
    let mut other = full(4);
    other.session_id = "00000000-0000-4000-8000-0000000000c3".to_owned();
    let outcome = replica.admit_frame(&other, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "got {outcome:?}"
    );
    assert_eq!(replica.session_id, SESSION);
}
