//! The staging bound itself, at both moments that matter: where the bytes
//! arrive, and the swap that replays them into a record that now exists. Mirrors
//! `session/binding.rs`; the adoption refusals are in `session_adoption.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::{Arc, Mutex};

use roost_worker::session::binding::{ChannelDelivery, RESUME_STAGE_CAP_BYTES, RecordBinding};
use roost_worker::session::resume::AdoptRefusal;

use session_support::{Harness, PinnedClock, SESSION, ScriptedKeeper, session_id};

/// A SURVIVOR THAT PRODUCED MORE THAN THE STAGING BOUND DURING ITS REBUILD IS
/// REFUSED, not adopted with a hole. The bytes past the bound are gone, and the
/// only repair that preserves a contiguous parser is to start again.
#[test]
fn an_adoption_past_the_staging_bound_is_refused_rather_than_truncated() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    let harness = Harness::with_keeper(Arc::clone(&keeper));
    // The keeper starts delivering the moment it is reattached, which is before
    // this worker has a record to put the bytes in.
    let binding = keeper.delivered();
    let chunk = vec![b'x'; 64 * 1024];
    for _ in 0..5 {
        binding.on_output(&chunk);
    }
    let refused = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .expect_err("a stream with a hole in it is not adopted");
    assert!(
        matches!(refused, AdoptRefusal::StagingOverflow { cap, .. } if cap == RESUME_STAGE_CAP_BYTES),
        "the refusal names the bound: {refused}"
    );
    assert_eq!(
        harness
            .table
            .with_record(&session_id(SESSION), |record| record.head_seq),
        None,
        "nothing is left installed, so no live session has a hole in its grid"
    );
    assert_eq!(keeper.killed(), vec![7], "the survivor is killed instead");
    assert!(
        harness.delivery.parsed.lock().expect("held").is_empty(),
        "no truncated prefix was parsed into anything"
    );
}

/// THE BOUND AT THE SWAP, not at the arrival. Bytes are pushed past the cap
/// through the same binding a keeper delivers into, and the question this asks
/// is the one the read path cannot answer: when the record finally exists, is
/// the hold replayed as though it had not overflowed? Deleting the `overflowed`
/// re-read inside `go_live` fails this and nothing else.
#[test]
fn a_hold_over_the_bound_is_not_replayed_at_the_swap() {
    let harness = Harness::new();
    let binding = RecordBinding::staged(
        7,
        Arc::clone(&harness.table),
        Arc::clone(&harness.delivery) as Arc<Mutex<dyn ChannelDelivery>>,
        Arc::new(PinnedClock),
    );
    let chunk = vec![b'x'; 32 * 1024];
    for _ in 0..9 {
        binding.on_output(&chunk);
    }
    assert_eq!(
        binding.staged_bytes(),
        9 * 32 * 1024,
        "the arrival path counted every byte it was handed"
    );
    assert!(
        !binding.go_live(),
        "the swap refuses a hold that went over the bound, even though the record \
         now exists and the bytes are already in memory"
    );
    assert!(
        harness.delivery.parsed.lock().expect("held").is_empty(),
        "nothing was parsed: the gap is not repaired by parsing the suffix"
    );
    assert_eq!(
        binding.abandon(),
        9 * 32 * 1024,
        "and the refused hold is dropped whole rather than trimmed"
    );
}

/// AND THE ADMITTING TWIN AT THE SWAP: a hold inside the bound is replayed
/// whole, in arrival order, and the binding is live afterwards.
#[test]
fn a_hold_inside_the_bound_is_replayed_whole_at_the_swap() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    let binding = RecordBinding::staged(
        7,
        Arc::clone(&harness.table),
        Arc::clone(&harness.delivery) as Arc<Mutex<dyn ChannelDelivery>>,
        Arc::new(PinnedClock),
    );
    binding.on_output(b"first");
    binding.on_output(b"second");
    assert!(binding.is_staged(), "a hold is holding until the swap");
    assert!(binding.go_live(), "two chunks is well inside the bound");
    assert_eq!(
        harness.delivery.parsed.lock().expect("held").clone(),
        vec![b"first".to_vec(), b"second".to_vec()],
        "replayed in arrival order, neither merged nor dropped"
    );
    assert!(!binding.is_staged());
    binding.on_output(b"third");
    assert_eq!(
        harness.delivery.parsed.lock().expect("held").len(),
        3,
        "after the swap the binding delivers straight through"
    );
}
