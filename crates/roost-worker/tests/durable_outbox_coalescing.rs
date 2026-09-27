//! The outbox's VOLATILE producers' fold: one record per key, replaced in
//! place. A link applying backpressure must not accumulate every version of one
//! agent status and then ship them all in order, because the coordinator would
//! walk a replacement edge it has already passed.
//!
//! Nothing here touches the durable file, which is why it needs no fixture and
//! no scratch directory: this is the lane that has no business being durable at
//! all. The rows' own survival is `durable_outbox.rs`, and the room a session
//! claims for its close is `durable_outbox_claims.rs`; all three were one suite
//! until it outgrew one file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_worker::outbox::{Admitted, Lane, Outbox};

/// The volatile producers' fold: one record per key, replaced in place. A link
/// applying backpressure must not accumulate every version of one agent status
/// and then ship them all in order, because the coordinator would walk a
/// replacement edge it has already passed.
#[test]
fn a_coalescing_frame_replaces_its_own_predecessor_in_one_lane() {
    let mut outbox = Outbox::default();
    let now = std::time::Instant::now();
    assert_eq!(
        outbox.admit_coalescing("s-1", Lane::Control, vec![0; 8], "first", now),
        Ok(Admitted::Queued)
    );
    assert_eq!(
        outbox.admit_coalescing("s-1", Lane::Control, vec![1; 8], "second", now),
        Ok(Admitted::Coalesced)
    );
    assert_eq!(
        outbox.admit_coalescing("s-2", Lane::Control, vec![2; 8], "other", now),
        Ok(Admitted::Queued)
    );

    assert_eq!(
        outbox.frame_count(),
        2,
        "one record per key, not one per version"
    );
    assert!(outbox.coalesces(Lane::Control, "s-1"));
    assert!(outbox.coalesces(Lane::Control, "s-2"));

    let drained = outbox.drain_all(now);
    assert_eq!(drained.len(), 2);
    assert_eq!(
        drained[0].bytes,
        vec![1; 8],
        "the replaced version is the one that went"
    );
    assert_eq!(drained[0].label, "second");
    assert_eq!(
        drained[1].bytes,
        vec![2; 8],
        "a different key's record is untouched"
    );
}

/// A frame that does not fit beside the one it would replace is refused, and the
/// older record STAYS. Refusing is the only truthful answer: a title the
/// coordinator has already been told is stale, but a refusal is a refusal.
#[test]
fn a_coalescing_frame_that_does_not_fit_keeps_the_record_it_would_replace() {
    let mut outbox = Outbox::new(8, 16);
    let now = std::time::Instant::now();
    // A second key fills the cap, so the replacement has to be judged against a
    // queue that is already full rather than against an empty one.
    outbox
        .admit_coalescing("s-0", Lane::Control, vec![9; 8], "other", now)
        .expect("admitted");
    outbox
        .admit_coalescing("s-1", Lane::Control, vec![0; 8], "first", now)
        .expect("admitted");
    let refused = outbox.admit_coalescing("s-1", Lane::Control, vec![1; 12], "second", now);
    assert!(
        refused.is_err(),
        "a 12-byte record replaced 8 bytes in a 16-byte queue that already held 8"
    );
    assert_eq!(
        outbox.byte_count(),
        16,
        "the refused record changed the byte count"
    );
    let drained = outbox.drain_all(now);
    assert_eq!(
        drained[1].label, "first",
        "the record that was told is the one that left"
    );
}
