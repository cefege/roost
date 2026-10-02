//! What a replica hands a renderer between paints: the deltas it folded since
//! the renderer's last painted revision, with the history they appended, which
//! the normalized canonical no longer carries. Pins
//! `terminal::renderer_deliveries` through `TerminalSession`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::Admission;
use roost_client_core::terminal::renderer_deliveries::MAX_PENDING_DELTA_FRAMES;
use support::{delta, full, replica_with_baseline, row, sync_token};

#[test]
fn a_renderer_behind_by_deltas_receives_them_with_their_appended_history() {
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);
    let painted = replica.frame_revision();
    let mut scrolled = delta(1, 4, 3);
    scrolled.scrollback_append = vec![row(0, "left the grid")];
    scrolled.scrollback_total = 1;
    assert_eq!(
        replica.admit_frame(&scrolled, false, &token, 10),
        Admission::DeltaApplied
    );
    assert!(
        replica.canonical().unwrap().scrollback_append.is_empty(),
        "the canonical is normalized: only the log still carries the row"
    );
    let owed = replica.deltas_since(painted).unwrap();
    assert_eq!(owed.len(), 1);
    assert_eq!(owed[0].scrollback_append[0].spans[0].text, "left the grid");
    assert_eq!(
        replica
            .deltas_since(replica.frame_revision())
            .map(<[_]>::len),
        Some(0),
        "a renderer that painted the current revision is owed nothing"
    );
}

#[test]
fn no_delta_reaches_back_across_a_full() {
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);
    let before_full = replica.frame_revision() - 1;
    assert!(
        replica.deltas_since(before_full).is_none(),
        "a renderer older than the baseline must paint the full"
    );
    replica.admit_frame(&delta(1, 4, 0), false, &token, 10);
    let painted = replica.frame_revision();
    let mut second = full(4);
    second.seq = 5;
    assert_eq!(
        replica.admit_frame(&second, false, &token, 11),
        Admission::BaselineReplaced
    );
    assert!(replica.deltas_since(painted).is_none());
    assert_eq!(
        replica
            .deltas_since(replica.frame_revision())
            .map(<[_]>::len),
        Some(0)
    );
}

#[test]
fn a_renderer_further_behind_than_one_batch_takes_the_full() {
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);
    let painted = replica.frame_revision();
    for base_seq in 1..=MAX_PENDING_DELTA_FRAMES as u64 + 1 {
        assert_eq!(
            replica.admit_frame(&delta(base_seq, 4, 0), false, &token, 10),
            Admission::DeltaApplied
        );
    }
    assert!(replica.deltas_since(painted).is_none());
    let owed = replica.deltas_since(painted + 1).unwrap();
    assert_eq!(owed.len(), MAX_PENDING_DELTA_FRAMES);
    assert_eq!(
        owed[0].base_seq, 2,
        "the retained suffix starts where it says"
    );
}

#[test]
fn a_refused_frame_moves_neither_the_revision_nor_the_log() {
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);
    let painted = replica.frame_revision();
    let refused = replica.admit_frame(&delta(7, 4, 0), false, &token, 10);
    assert!(matches!(refused, Admission::Refused { .. }), "{refused:?}");
    assert_eq!(replica.frame_revision(), painted);
    assert_eq!(replica.deltas_since(painted).map(<[_]>::len), Some(0));
}
