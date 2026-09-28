//! A kill reaches the worker it names, connected or not, and two workers'
//! links never cross.
//!
//! The two halves that matter: a reap runs when a snapshot commits and the
//! worker that owns the dead PTY is OFFLINE at that moment, so a kill delivered
//! only to a live link is a kill delivered to nobody; and a kill is addressed to
//! a FINGERPRINT, so a registry that held one outbox would aim a second
//! worker's kill at the first.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex, PoisonError};

use roost_coord::terminal_screen::live_effects::OrphanPtyKill;
use roost_coord::terminal_screen::orphan_kills::{
    LiveOrphanKills, MAX_RECORDED_KILLS_PER_WORKER, PendingKill,
};
use roost_protocol::wire::WorkerFp;

fn worker(byte: char) -> WorkerFp {
    WorkerFp::try_from(byte.to_string().repeat(64)).expect("a 64-hex fingerprint")
}

fn outbox() -> Arc<Mutex<Vec<PendingKill>>> {
    Arc::new(Mutex::new(Vec::new()))
}

fn drain(box_: &Arc<Mutex<Vec<PendingKill>>>) -> Vec<PendingKill> {
    let mut held = box_.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::take(&mut *held)
}

fn ids(kills: &[PendingKill]) -> Vec<&str> {
    kills.iter().map(|kill| kill.session_id.as_str()).collect()
}

#[test]
fn a_kill_for_an_offline_worker_is_recorded_and_delivered_on_attach() {
    let kills = LiveOrphanKills::new();
    let mine = worker('a');
    kills.kill(&mine, "session-1");
    kills.kill(&mine, "session-2");
    assert_eq!(kills.owed_count(), 2);

    let socket = outbox();
    let delivered = kills.attach(&mine, Arc::clone(&socket));

    assert_eq!(ids(&delivered), vec!["session-1", "session-2"]);
    assert_eq!(kills.owed_count(), 0, "delivered kills are not owed twice");
    assert!(
        drain(&socket).is_empty(),
        "attach drains; it does not also send"
    );
}

#[test]
fn two_connected_workers_never_receive_each_others_kills() {
    // THE TEST THE SINGLE-OUTBOX MODEL COULD NOT EXPRESS. A kill is addressed to
    // a fingerprint, so a registry holding one link would hand worker B's reap
    // to worker A's socket — and a wrong terminal kill destroys a live PTY.
    let kills = LiveOrphanKills::new();
    let (mine, theirs) = (worker('a'), worker('b'));
    let mine_socket = outbox();
    let theirs_socket = outbox();
    kills.attach(&mine, Arc::clone(&mine_socket));
    kills.attach(&theirs, Arc::clone(&theirs_socket));
    assert_eq!(kills.connected_workers(), 2);

    kills.kill(&mine, "mine-1");
    kills.kill(&theirs, "theirs-1");

    assert_eq!(ids(&drain(&mine_socket)), vec!["mine-1"]);
    assert_eq!(ids(&drain(&theirs_socket)), vec!["theirs-1"]);
    assert_eq!(
        kills.owed_count(),
        0,
        "both were delivered to the right link"
    );
}

#[test]
fn a_kill_owed_to_one_worker_is_not_delivered_to_another() {
    let kills = LiveOrphanKills::new();
    let (mine, theirs) = (worker('a'), worker('b'));
    kills.kill(&mine, "mine");
    kills.kill(&theirs, "theirs");

    let socket = outbox();
    assert_eq!(ids(&kills.attach(&mine, Arc::clone(&socket))), vec!["mine"]);
    assert_eq!(kills.owed_count(), 1, "the other worker's kill stays owed");
    assert!(drain(&socket).is_empty());
}

#[test]
fn a_link_that_ends_goes_back_to_recording_rather_than_delivering_into_the_void() {
    let kills = LiveOrphanKills::new();
    let mine = worker('a');
    let socket = outbox();
    kills.attach(&mine, Arc::clone(&socket));
    kills.detach(&mine, &socket);
    assert_eq!(kills.connected_workers(), 0);

    // Without the detach this would push into an outbox nobody reads, and a
    // reconnecting worker would find it empty and believe it owed nothing.
    kills.kill(&mine, "after-detach");
    assert!(
        drain(&socket).is_empty(),
        "a detached link receives nothing"
    );
    assert_eq!(kills.owed_count(), 1, "and the kill is owed again");
    assert_eq!(
        ids(&kills.attach(&mine, Arc::clone(&socket))),
        vec!["after-detach"]
    );
}

#[test]
fn a_superseded_links_late_detach_leaves_its_replacement_attached() {
    // A reconnect whose old socket closes AFTER the new hello attached: the old
    // link's detach must not remove the new link's outbox, or the new link's
    // kills are recorded as owed and never carried (v2 `_deleteIfStillMine`).
    let kills = LiveOrphanKills::new();
    let mine = worker('a');
    let superseded = outbox();
    let replacement = outbox();
    kills.attach(&mine, Arc::clone(&superseded));
    kills.attach(&mine, Arc::clone(&replacement));
    kills.detach(&mine, &superseded);

    kills.kill(&mine, "after-reconnect");
    assert_eq!(ids(&drain(&replacement)), vec!["after-reconnect"]);
    assert_eq!(
        kills.owed_count(),
        0,
        "a live link's kill is carried, not owed"
    );
}

#[test]
fn the_record_is_bounded_per_worker_and_drops_the_oldest_first() {
    let kills = LiveOrphanKills::new();
    let (mine, theirs) = (worker('a'), worker('b'));
    for index in 0..(MAX_RECORDED_KILLS_PER_WORKER + 8) {
        kills.kill(&mine, &format!("session-{index}"));
    }
    kills.kill(&theirs, "unbounded-neighbour");
    assert_eq!(
        kills.owed_count(),
        MAX_RECORDED_KILLS_PER_WORKER + 1,
        "the bound is PER WORKER, so one worker's backlog cannot consume another's"
    );

    let delivered = kills.attach(&mine, outbox());
    // The newest kill is the one whose session row most recently became wrong,
    // so it is the one that survives when the bound forces a choice.
    assert_eq!(
        delivered.first().expect("a survivor").session_id,
        "session-8"
    );
    assert_eq!(
        delivered.last().expect("the newest survives").session_id,
        format!("session-{}", MAX_RECORDED_KILLS_PER_WORKER + 7)
    );
}
