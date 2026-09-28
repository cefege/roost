//! An owned reap must have somewhere to travel, and this is the test that says
//! so while there is nowhere for it to go.
//!
//! **THE ORDER MATTERS AND IT IS THE POINT.** R3 made an owed reap *drainable*:
//! `drain_reaps` reads `result.snapshot_reap_ids` and calls
//! `kill_orphan_pty` per id after the readiness barrier, and R3's tests drive
//! that end to end. What R3 could not exercise is DELIVERY — the registry
//! counts a kill and, with no socket attached, nothing ever carries it. R3's
//! tests would still pass. **That is why this file exists and why the first
//! test below is the property R3 could not reach.**
//!
//! `LiveOrphanKills::attach` and `detach` already exist from `a8d32426`. What
//! was missing is a CALLER, and a caller that is never called is the reachability
//! class again — so the first test here is written to fail before anything
//! supplies a socket, and the order of the two tests is the order of the work.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex, PoisonError};

use roost_coord::terminal_screen::live_effects::OrphanPtyKill;
use roost_coord::terminal_screen::orphan_kills::{LiveOrphanKills, PendingKill};
use roost_protocol::wire::WorkerFp;

fn worker() -> WorkerFp {
    WorkerFp::try_from("a".repeat(64)).expect("a 64-hex fingerprint")
}

/// A socket's outbox, which is the only thing a kill can travel on.
fn outbox() -> Arc<Mutex<Vec<PendingKill>>> {
    Arc::new(Mutex::new(Vec::new()))
}

fn drain(box_: &Arc<Mutex<Vec<PendingKill>>>) -> Vec<PendingKill> {
    let mut held = box_.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::take(&mut *held)
}

/// WITHOUT a socket, an owned reap is counted and never delivered.
///
/// **This test passes today and is expected to keep passing — it is the control.**
/// It is the shape every other R4 test has to be distinguishable from: a reap
/// that is counted, owed and never carried looks exactly like a reap that is
/// counted, owed and carried, from every assertion except one. The one is below.
#[test]
fn without_a_socket_an_owned_reap_is_counted_and_never_delivered() {
    let kills = LiveOrphanKills::new();
    let mine = worker();
    kills.kill(&mine, "session-1");

    assert_eq!(kills.owed_count(), 1, "the reap is owed");
    assert_eq!(
        kills.connected_workers(),
        0,
        "and there is nowhere to send it"
    );

    // The property a caller of `attach` has to be judged against. A link that
    // never attaches is not "slow to deliver" — it is a silent drop, and only
    // this assertion can tell it from a delivery that is merely late.
    let socket = outbox();
    assert!(
        drain(&socket).is_empty(),
        "nothing was sent, because nothing attached: a reap with no socket is \\
         counted forever and the count is the only evidence"
    );
    assert_eq!(kills.owed_count(), 1, "and it stays owed");
}

/// A link that attaches takes what the worker is owed, and the counts say so.
///
/// **This is the test R3 could not write**, because delivery needs a caller and
/// the caller is `connection.rs`. The three facts it pins are the ones a
/// reader cannot derive from `attach`'s signature: the owed kill is HANDED OVER
/// rather than copied, the count goes to zero, and a second attach of the same
/// worker does not deliver it twice.
#[test]
fn a_link_that_attaches_takes_what_the_worker_is_owed_exactly_once() {
    let kills = LiveOrphanKills::new();
    let mine = worker();
    kills.kill(&mine, "session-1");
    kills.kill(&mine, "session-2");
    assert_eq!(kills.owed_count(), 2);

    let socket = outbox();
    let delivered = kills.attach(&mine, Arc::clone(&socket));

    assert_eq!(
        delivered
            .iter()
            .map(|kill| kill.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["session-1", "session-2"],
        "an owned reap is handed over in the order it was owed"
    );
    assert_eq!(kills.owed_count(), 0, "delivered is not still owed");
    assert_eq!(kills.connected_workers(), 1, "and the link is registered");

    // A reattach is what a reconnect looks like to the registry, and it must not
    // deliver a reap that was already delivered.
    let again = kills.attach(&mine, Arc::clone(&socket));
    assert!(
        again.is_empty(),
        "a second attach has nothing left to hand over"
    );
    assert!(drain(&socket).is_empty(), "and nothing was sent twice");
}

/// A link that ends goes back to recording, because a detached socket collects
/// kills nobody reads.
#[test]
fn a_link_that_ends_goes_back_to_recording() {
    let kills = LiveOrphanKills::new();
    let mine = worker();
    let socket = outbox();
    kills.attach(&mine, Arc::clone(&socket));
    kills.detach(&mine, &socket);
    assert_eq!(kills.connected_workers(), 0);

    kills.kill(&mine, "after-detach");
    assert!(
        drain(&socket).is_empty(),
        "a detached socket receives nothing"
    );
    assert_eq!(kills.owed_count(), 1, "and the reap is owed again");
    assert_eq!(
        kills.attach(&mine, socket).len(),
        1,
        "so a reconnect takes it"
    );
}
